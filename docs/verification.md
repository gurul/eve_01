# Formal verification (Lean 4)

Thirty-one models of eve's state machines are in Lean 4 under `verification/`, with 539
kernel-checked theorems. eve lite (2026-10-13, below) added one model and 49 theorems; before it there were
six from the first pass, eight for the watcher (2026-09-25), four for
the Voice PE controller (2026-09-26), one for the OpenAI seam (2026-09-28), one for the
chief of staff (2026-09-29), five for calls, Eve Link and the Fire TV (2026-10-04), and four
for the apps' policy, the routed turn, Share and the calendar (2026-10-05), and one for a
"new claude" joining late (2026-10-06, below). Four theorems were added on 2026-10-09 to the
apps' and the routed turn's models, for that day's Python (below).
The routine-confirmation update adds sixteen more across AppsPolicy and Chief.
AppsPolicy's current model distinguishes routine private operations from consequential
ones, preserves explicit read/ask policies, and requires a complete approval for
consequential writes. Calendar preparation preserves an explicit notification choice
and prevents guest delivery after a verified private edit even if guests change before
execution. Chief permits routine delegated phases while retaining the executor's live
approval callback for consequential steps. Python tests cover provider JSON, actual
request parameters and callback integration; these are outside the abstract proofs.
A model of code that had a bug has two kernel-checked theorems:

- **`current_violates`** — a concrete trace on a model of the code *as it was* on
  2026-09-25 (commit `0b7d36d`) after which the property is false: the bug, proved.
- **`fixed_invariant`** — the property holds on a model of the fixed code for **every**
  trace (induction over an arbitrary event list), not just the counterexample.

Every fix also has a pytest in `bridge/tests/` that replays the counterexample against the
real Python classes. Each replay was run before the fix and failed; it passes now.

`check-all.mjs` also accepts a third, honest shape, for code that was right from the start:
**`code_invariant`**, the property proved of the code as it is.

## The first six models

| Model | Code | Property | What the counterexample was |
|---|---|---|---|
| `Eve/Command.lean` | `matchers.py` `classify_command` | "allow" answers only one simple command whose program cannot run another command or write a file — for **every** `matchers.toml` | The built-in `^echo( |$)` allowed `echo x; rm -rf ~`. Also `ls && curl … \| sh`, `ls $(…)`, `env rm -rf /`, `find -execdir`, `fd -x`, `rg --pre`, `tree -o`, `git log --output`. The daemon turned that "allow" into an explicit allow before Jev or the phone saw it. |
| `Eve/Questions.lean` | `telegram.py` `_ask_user` / `decide_permission` | while any question waits, the typed-answer slot names the newest one still waiting | Three nested questions A, B, C; B then C end; the slot empties while A still waits, so a typed yes for A went to Claude. |
| `Eve/TaskStop.lean` | `telegram.py` `_run_agent` / `_stop` | a task that completed is never reported "Stopped.", and its result is sent | A typed "stop" after the agent returned (while the progress message closed) said "Stopped." and ate the result. |
| `Eve/Forget.lean` | `memory.py` `forget_apply`, `records.py`, `dream.py`, `mem0_memory.py` | once forget completes, no interleaving with the dream or the index ingest puts a forgotten line back in the records, git or the index | A forget during the dream's model call was undone by the dream's write, and its commit put the words back into the history the forget had squashed. Same for the mem0 ingest. |
| `Eve/Serial.lean` | `serial_transport.py`, `daemon.py` watchdog and ack router, `folder_push.py` | (A) at most one write on the port at a time; (B) a flapping link reaches the RTS reset; (C) every ack reaches its own request's waiter | (A) a sender cancelled by `wait_for` freed the lock while its thread still wrote; (B) the reconnect's status ack counted as a clean poll, so a link that flapped never escalated to the RTS reset; (C) an ack that arrived before its waiter registered was dropped, and a late chunk ack answered the next chunk. |
| `Eve/Voice.lean` | `voice_agent.py` `_backend_event`, `_ask_user` | once nothing is in flight the face is not stuck on thinking/speaking; a question slot is always released | A backend stream `error` left the response "active" forever: the face held "thinking" and every later reply was held back. A failed send of a spoken question left the slot set, blocking the idle close and the goodbye. |

## The watcher's eight models

Written by an ultracode workflow (2026-09-25):
- four modelers, one per subsystem
- an adversarial verifier per model, which re-ran the checker, compared the Lean
  step functions with the Python line by line, and re-ran every replay
- two code reviewers, whose own findings became four more models

Every model found a bug, and the Python fix is each model's `fixed_invariant`
step. A second workflow then checked each fixed step against the Python as
written:
- four auditors, one per two models
- two adversarial reviewers of the code written after the first pass
- a completeness critic

It found where the Python goes beyond its model. Each deviation is now written
in its model's docstring:
- the 1e-9 tolerance on percentages
- a first reading the owner never saw
- the fix for a check that raises, which sits in `_run_one` and `add`
- notes made plain for every kind
- the exponent cap and OpenRouter's hold
- the IPv6 prefixes, the one-request proxy and the process-tree kill

It also found new bugs, all fixed, each with a replay in
`test_watch_reverify.py`.

The watcher's code was not committed while it was modelled, so its
`current_violates` traces refer to the working tree of 2026-09-25, not to a commit.
The line numbers in the docstrings are those of that tree. The functions they name
are current.

| Model | Code | Property | What the counterexample was |
|---|---|---|---|
| `Eve/WatchLimiter.lean` | `watch.py` `RateLimiter`, the gate in `_run_one` / `add` | the bucket never exceeds its burst and matches a monotone-clock reference (a clock that steps back mints nothing); no read goes to a host inside its hold; the model cap counts every day once | a second 429 shortened a Retry-After hold still in force; a check let through before a 429 landed read the host during its hold; a day label the clock stepped back onto got a fresh model cap |
| `Eve/WatchConditions.lean` | `watch.py` `evaluate` | no alert on a reading the owner saw at add time; an edge condition alerts at most once per false-to-true transition; a percentage alert fires exactly at the move and re-bases | a zero baseline made `drop_pct` / `rise_pct` raise `ZeroDivisionError` (and stopped the tick); an exact move (3.00 to 2.70) missed by float rounding |
| `Eve/WatchScheduler.lean` | `watch.py` `tick`, `add`, `remove`, `check`, `_run_one` | ids are unique and never reused; a removed watch texts nothing; the via ladder only moves forward; "I can't read" is said once per streak; a new watch wakes the loop | a watch removed mid-check still texted its alert; one watch whose check raised anything but `FetchError` stopped every watch after it; removing the newest watch handed its id to the next |
| `Eve/WatchTicketmaster.lean` | `watch.py` `ticketmaster_reading`, `pick_attraction`, `evaluate` | available iff an event is onsale or rescheduled, or a presale window is open now; an ended sale is never listed; no price is `None`, never 0; the owner hears once when the sale opens | a sale already open at the first background check (the add's check failed) was never texted; "The Jimi Hendrix Experience" was dropped as a tribute act; every game ("A vs. B") was dropped |
| `Eve/WatchStarve.lean` | `watch.py` `tick`, `_run_one` | every watch due when a tick starts is taken up by that tick | a read that raised `LookupError` (an unknown charset) left `next_at` unchanged and aborted the tick, every time |
| `Eve/WatchSsrf.lean` | `watch.py` `http_request`, `tls_request`, `render` | every connection the watcher opens goes to a public address | DNS rebinding between the check and the connect; the browser's redirects, WebSockets and popups, which `page.route` never sees |
| `Eve/WatchLink.lean` | `watch.py` `model_reading`, `evaluate` | a page alert links only the owner's page; a search alert only the search's link; no link hides behind words | a page's words talked the reader model into a URL that replaced the owner's link in eve's alert |
| `Eve/WatchRoute.lean` | `telegram.py` `_handle` | "/watch <words>" is never typed into the Claude session or sent to Codex | with a relay on, "/watch AAPL below 300" from eve's own menu was typed into the terminal |

The replays are `bridge/tests/test_watch_lean_*.py`, `test_watch_security.py`,
`test_watch_shapes_replay.py` and `test_watch_reverify.py`. The replays that start a real
Chromium are marked `live` and run with `EVE_LIVE=1`: the guard test, its negative
control, and the overlay-navigation test. The render-deadline tests use a stand-in child
process and run in the normal suite.

## The Voice PE controller's four models

The owner asked for Lean proofs before these changes are pushed (2026-09-26). Each model
found a bug. Three were caught first by a pytest or seen live, and the Lean
`current_violates` restates them. The desk-call model found four more that nothing had
caught. The replays are named in each row.

| Model | Code | Property | What the counterexample was |
|---|---|---|---|
| `Eve/Pacing.lean` | `desk_call.py` `BoardSpeaker.play` | the daemon is never more than `LEAD_SECS` (3 s) ahead of the board's playback, for any piece size up to the lead | waiting until 3 s ahead and then sending another 100 ms piece left it 3.1 s ahead (`test_the_daemon_stays_at_most_three_seconds_ahead_of_the_board`) |
| `Eve/ControllerRoute.lean` | `controller.py` `mirror`, `_replay`, `_on_message`; the tee in `daemon.py` | the controller is never sent sound-on (beeps only on the StackChan); only `ptt`, `key` and `focus` from it reach the daemon | the first version mirrored `{"cmd":"sound","on":true}` to the Voice PE, live or on the next connect (`test_the_beeps_are_the_robots_alone`) |
| `Eve/PortPick.lean` | `serial_transport.py` `_resolve_port` | the robot's glob never opens a node whose USB serial is skipped; a `usbsn:` pick is that serial's node | seen live: `/dev/cu.usbmodem101` (the Voice PE) sorted before `31201` (the StackChan) and was opened as the robot. The model's docstring also named a gap it could not cover: `comports()` raising skipped the skip filter. With a skip set, that now waits instead of guessing (`test_two_esp32_boards_are_told_apart_by_usb_serial`, `test_when_the_usb_list_fails_a_skip_is_never_guessed_past`) |
| `Eve/DeskCall.lean` | `desk_call.py` `DeskCalls`, `phone_call.py` `Call.run`, `telegram.py` `listen`, `daemon.py` `ptt` and `_wake_suppressed` | the desk mic is open only while the button is held; the chat's one call listener is never cleared under a live call; the wake word stays off while the button is held | four traces: a desk call ending cleared a phone call's reader, so the phone call went silent; a quick tap opened the mic after its release; a release during a conversation was dropped, leaving the mic open; a call ending mid-hold turned the wake word back on (`bridge/tests/test_desk_call_lean.py`). Since the hearing-call fix (CallScope), a call whose words are heard takes the listener back (`deskHeard`, `phoneHeard`); the properties hold with these events too |

## The OpenAI seam (2026-09-28)

A model written with the code it checks. It has a `current_violates` on code that shipped
(the OpenAI seam before its retry, which died on one TLS drop), and also proves the obvious
alternative wrong. The replays are `bridge/tests/test_response_retry_lean.py` (against the
real `make_stream_creator` with a fake client).

A second model from that day, for an alternative desktop executor's steering, was removed
with that executor on 2026-09-30 at the owner's request; Codex is the only desktop executor.

| Model | Code | Property | What the counterexample was |
|---|---|---|---|
| `Eve/ResponseRetry.lean` | `computer_agent.py` `make_response_creator`, `make_stream_creator` | no text is read out twice; a turn fails only after a drop on each of three tries, or a drop after text was read out | `[drop]`: one `ssl.SSLError` mid-read killed the turn (three phone-call turns on 2026-09-28). The naive cure, "always retry", is proved wrong too: `[text, drop, text]` reads the sentence out twice. The first fix retried once, at once, on the same client; `once_violates` is `[drop, drop]`, the 2026-09-30 11:11 Telegram turn whose retry dropped 120 ms later. The fix retries twice (0.3 s, then 1 s, each on a new client), only while nothing has been read out; `fixed_survives_two_drops` replays that trace to success. The new-client part is tested, not modelled (`bridge/tests/test_response_creators.py`) |

## The chief of staff (2026-09-29)

`Eve/Chief.lean` was written before the chief's Python, from its design, so it has the
third shape: one `code_invariant`, proved over every trace. There was never a buggy version
to put a `current_violates` on.

The model is one card, the Desk it shares with every other card, and the lesson store. It
has one step function over 23 events:
- the owner's taps and words: a Go, "go c<N>", Change, Reopen, Drop, "done c<N>", a raise;
- the executors' results, and the oracles' readings of the evidence;
- a restart, and "It happened" / "It didn't";
- Reflexion: a lesson, and a retry;
- the clock, a new date, breakpoints, and `Desk.offer`.

`code_invariant` is the conjunction of seven properties:

| Property | What it says |
|---|---|
| DoneNeedsEvidence | A card is done only on the owner's word, or when every done check is confirmed. Every confirmation must rest on evidence the oracle saw outside the executor's own sentence. |
| OneYesOneAct | Every one-way dispatch spends its own yes, given for that phase at the card's revision at that moment. A no, a hold word or a 180 s timeout is never a yes. A revision cancels a yes that has not been spent. |
| BudgetBeforeDispatch | What the card spent, plus the ceiling held by its running phase, never exceeds the cap. The only excess allowed is what an executor ran past its own ceiling. |
| PushBudgetQuiet | An unprompted push, or a reminder the owner asked for, goes out now only outside 22:30–08:00 and at a breakpoint; a reminder also once it has waited 45 minutes for one (2026-10-06, `waited_reminder_goes`). An unprompted push also needs pushes on, and at most 2 go out a day. The day's count survives a restart. No Go is asked, and no act or one-way phase starts, in quiet hours. |
| NoActReplayAfterRestart | A one-way act that was in flight at a restart never runs again without a yes given after the restart. |
| OneWayNeverAutoRetried | A one-way phase's attempt count rises only with a fresh yes. A two-way phase retries itself at most twice (3 trials). |
| LessonsBounded | At most 3 lessons are kept per (executor, phase kind) key: Reflexion's Omega (Shinn et al., NeurIPS 2023). |

The positive control is `naive_violates`. The same machine with an oracle that takes the
executor's "done" as evidence closes a research card as done with no evidence. With the
code's oracle, `executor_word_is_unverified` shows the same trace ends unverified.

Worked traces, each checked by `decide +kernel`:
- the standing-desk job of the design closes as done, with one yes for one act;
- a timed-out Go approves nothing;
- a revision cancels the yes;
- after a restart, "It didn't" runs nothing until a new yes;
- a failed one-way act waits for a Go even when the reflection says retry;
- a two-way phase stops after three trials;
- the third unprompted push of a day is batched, across a restart;
- an approved act does not start at 23:00.

The replay, `bridge/tests/test_chief_lean.py`, holds the Python to the model:

- **The rules.** The model's step function is copied into Python, rule by rule, each citing
  its line in `Chief.lean`. The constants (quiet hours, push budget, retries, Omega) are read
  from the `.lean` file and compared with the code's. The copy must reproduce every worked
  trace above, and `quiet` must equal the desk's reading for all 1,440 minutes of a day.
- **The traces.** 240 seeded traces, of 40 moves each, drive the real `Chief` with fakes.
  The moves are:
  - each executor's result: a web read with four links, a failed read, an options sheet, a
    reply that is not one, an act that saw the confirmation on screen, an act whose only claim
    is its own sentence, and a failed act;
  - every owner answer and code word;
  - the clock across the quiet-hour edges;
  - restarts.

  Every ledger line becomes a model event. After every move, the step started, every yes and
  every Go asked, each check's verdict (set against the evidence the fakes produced), the
  card's status, revision, steps, attempts, retries and lessons must agree, and the model's
  `Spec` must hold. A coverage test checks that the traces reached every rule.
- **The desk.** 36 traces of 120 events run each of the three push modes through the model's
  `offer` and a desk reloaded from the same folder.
- **The positive control.** A variant of the code takes the Mac executor's sentence for a
  screen state. The replay stops at `ui_seen`: the code says confirmed, the model
  unverifiable. The same variant agrees with the model's `naiveVerdict`, and its end state
  breaks DoneNeedsEvidence, as `naive_violates` proves.

Where the code and the model differ in shape but not in the properties, the replay maps one
onto the other. Its docstring lists each case. Two examples:
- after a failed act, the model waits for a Go, while the code closes the card as unverified
  until Reopen;
- "go c12" on an act in doubt is It didn't then a Go in the model, and a Go by id in the code.

The replay also found a liveness gap, now fixed. A card closed while a web step ran, then
reopened, kept the step marked running and did not move until a restart. The step's late result
is now held and put on the step at Reopen, as the model does
(`test_a_card_closed_during_a_web_step_then_reopened_moves_on`).

## Calls, Eve Link and the Fire TV (2026-10-04)

Five models of the code merged at `2ad3550` (Fire TV, the Action Button, the rendezvous, and
"a call reads only call work"). Each was checked by an adversarial reviewer against the Python or
Swift line by line. Each model found bugs in `2ad3550`, except `TalkIntent`. Those bugs are
now **fixed** on branch `fix/lean-findings` (commit `1dd0f26`). An adversarial verifier then found
three gaps in `1dd0f26`, fixed in the commit after it: the hearing call and the relay's prompt order
(CallScope), and the address retry against a newer turn (LinkTurn). In each model, `current_violates`
and the `current_*` theorems model the code at `2ad3550`, except the theorems named for `1dd0f26`. The `Fix` step mirrors the fixed lines,
and `fixed_invariant` proves the property of the fixed code for every trace. Each model's header
gives the line numbers of both versions. The old code, where there is one, is `48df362`.

| Model | Code | Property | Finding, and the fix |
|---|---|---|---|
| `Eve/CallScope.lean` | `telegram.py` `_FROM_CALL`, `hear`, `_call_reader`, `_say`, `_send_choices`, `_think`, `_to_call`, `_spawn`, `_type_to_claude`, the "claude on" queue, the Claude relay (`relay_text`, `relay_notification`, `relay_tool_call`, `decide_permission`); `daemon.py` `_ipc_chat` | (P1) a line is read on a call only if its work began with words heard on a call; (P2) such work has its lines read while that call holds the listener; (P3) only on that call | `48df362`: a message typed in Telegram during a call had its answer read aloud (`old_violates`). `1c68787` fixed that (`current_p1_invariant`) but at `2ad3550`: (1) the Claude relay ran in a daemon task, so Claude's answer to a prompt spoken on the call was never read (`current_violates`, `current_relay_unread`); (2) `_FROM_CALL` was a Bool, so work outliving its call was read on the next call; (3) the Mac chat window went through `tg.hear`, so typed words were read; (4) the "claude on" queue replayed held messages with the wrong origin (`current_gaps`); (5) two docs described the old rule. **Fixed:** `_FROM_CALL` holds the call's reader and `_call_reader` reads only on that same call; `_type_to_claude` saves the prompt's call and the relay reads with it; `_ipc_chat` calls `hear(…, on_call=False)`; held messages keep their own origin; streamed sentences go through `_to_call`; `docs/stackchan/miniapp.md` describes the rule (the stick link's doc did too, until the stick was removed on 2026-10-07). P1-P3 hold for every trace (`fixed_invariant`). Replays in `bridge/tests/test_telegram.py`: `test_claudes_answer_to_a_prompt_spoken_on_the_call_is_read_on_it`, `test_work_begun_on_one_call_is_not_read_on_the_next`, `test_words_typed_in_the_mac_chat_window_are_not_read_on_an_open_call`, `test_a_typed_message_held_by_a_spoken_claude_on_is_not_read`, `test_words_said_while_a_typed_claude_on_lists_are_read`. **At `1dd0f26`** (`Mid`): (6) `hear` marked work with the listener's reader, not the hearing call's, so a desk press during a phone call was answered on the phone (`current_hearing_call`); (7) each relay line took the origin of the last prompt typed, at the flush, so a typed answer was read while a spoken prompt waited behind it, a spoken answer was lost the other way round, a batch straddling two prompts was lost whole, and a turn typed at the Mac was read (`current_relay_fifo`). **Fixed:** `Call.heard` passes `hear(…, call=self.say)`, and the hearing call takes the listener back; `_type_to_claude` queues each prompt with its origin, the UserPromptSubmit hook (`relay_turn_began`) pops it, and each line keeps the origin it arrived with (`_flush_relay` splits by origin). Replays: `test_words_heard_on_the_desk_are_read_on_the_desk_while_the_phone_holds_the_listener` (`test_desk_call_lean.py`), `test_a_typed_prompts_answer_is_not_read_while_a_spoken_prompt_waits_behind_it`, `test_a_spoken_prompts_answer_is_read_while_a_typed_prompt_waits_behind_it`, `test_a_relay_batch_that_straddles_two_prompts_is_read_by_each_prompts_origin`. |
| `Eve/TalkIntent.lean` | iOS `TalkIntent.perform`, `PhoneLink.press` / `pressAfterIntent`, `TalkActivity.ensureUp` / `show`, `Relay.press` / `phoneBegan` | (P1) the intent starts the microphone only while a Live Activity is live; (P2) a press that would start a turn is never silently dropped | `48df362`: `show` returned quietly when iOS refused the Live Activity, and `phoneBegan` started the microphone anyway, so App Intents trapped and the app quit (`[fireBg, reqFail, micStart]`). `2ad3550` is sound for every trace (`fixed_invariant`). The LinkTurn fixes moved Relay.swift's lines but changed nothing this model reads. The header gives the new line numbers. |
| `Eve/LinkTurn.lean` | iOS `Relay` (`setStick`, `refreshAddress`, `fromStick`, `fromEve` and its address retry, `holdBegan` / `holdEnded`, `phoneBegan`, `endStickTurn`), `TurnOwner` / `TurnHold`, `CallSocket`, the Hold to talk button; `phone_call.py` `_begin_press`, `_feed`, `done` | (P1) stick and phone frames never share one press (the stick was removed on 2026-10-07: the phone-only code is a restriction of the fixed model, so P2-P4 still hold and P1 is trivial); (P2) at most one automatic address retry between two opens; (P3) the retry restarts a held turn only while the finger is still down; (P4) the retry never starts its turn after a newer turn began or the call opened | `48df362` had P1 (`old_p1_invariant`), and `2ad3550` had P2 (`current_p2_invariant`). At `2ad3550` (`current_violates`): P1 failed because `setStick(false)` dropped a live stick press without a `done`. P3(a) failed because `holdBegan` on an Action Button turn left `lastEndsItself` true. P3(b) failed because a `holdBegan` that threw left `holdDown` true. **Fixed:** `setStick(false)` sends the stick's press through `endStickTurn()` (its batch, then `done`). `TurnHold.tookOverLiveTurn()` and `TurnHold.failed()` cover P3(a) and P3(b). P1-P3 hold for every trace (`fixed_invariant`). The replays (`p1Trace`, `p3TraceA`, `p3TraceB`) are in `ios/EveLink/Tests/main.swift`. They run the extracted `TurnOwner` / `TurnHold` decisions, not `Relay` itself. **At `1dd0f26`** (`Ver.mid`), (P4) failed: the retry awaiting `refreshAddress()` restarted the lost turn after the owner had started and finished a newer one (`current_retry_clobbers`, `p4Trace`). **Fixed:** `TurnGeneration` counts turns begun and opens, and the retry starts only if the count is the one it captured and no turn is live (`retryMayStart`). P1-P4 hold for every trace (`fixed_invariant`); `replayP4` in `Tests/main.swift`. |
| `Eve/FireTv.lean` | `firetv.py` `FireTV.shell`, `ensure`, `_recover`, `state`, `_send`, `_mute`, and every command sender | (P1) no command line ever contains KEYCODE_POWER (26); (P2) a mute answered ok leaves the TV at the target; (P3) an `again=False` command is delivered at most once, retried only after "gone", at most two attempts | P1, P3 and the weak P2 (ok *from a read*) held at `2ad3550` (`code_invariant`). The strict P2 did not (`current_violates`, `code_violates_unread_ok`, `code_violates_refused_key`). `_mute` answered ok when the readback failed, and it discarded the key's `rc`. **Fixed:** a refused key is not-ok, and an unread readback is not-ok. So ok needs the key taken AND the readback at the target. P1, strict P2 and P3 hold for every trace (`fixed_invariant`). The rc check goes one step past the first proposal (`proposed_fix_invariant`). Positive control: retrying on any loss sends a d-pad press twice (`naive_violates`). P3 assumes a "gone" answer means the command never ran. Replays in `bridge/tests/test_firetv.py`: `test_mute_lost_and_unread_is_not_ok`, `test_mute_refused_key_is_not_ok`, and the control `test_mute_dropped_key_with_a_readback_says_so`. |
| `Eve/Rendezvous.lean` | `miniapp.py` `_announce_loop`, `run`; `stick_link.py` `publish_address`, `LinkToken`; ntfy.sh and the phone's `Rendezvous.newest` | (P1) the topic's newest message is the current address; (P2) no repost of the same address within 6 h; (P3) a failed post is retried on the next poll; (P4) the loop stays alive | At `2ad3550`: (1) the loop recorded `self.url` after the post returned, not the address it posted, so a tunnel restart during a post left the old address on the topic for up to 6 h (`current_violates`, `current_posts_only_at_repost`). (2) `publish_address` caught only `OSError`, so `BadStatusLine` or a base URL with no scheme killed the loop silently (`current_dies`, `current_dead_forever`). Fixing (1) alone does not fix (2) (`url_fix_alone_dies`). **Fixed:** the loop reads `url = self.url` before the post and records `(url, token)`. `publish_address` catches any exception, and a `try` around each iteration also catches any exception. P1-P4 hold for every trace (`fixed_invariant`, `fixed_retries`). Replays in `bridge/tests/test_stick_link.py`: `test_the_announcer_records_the_address_it_posted_not_the_one_after`, `test_a_raising_post_is_a_failed_post_and_the_announcer_goes_on`, `test_publish_never_raises`. |

`test_a_typed_message_is_not_read_aloud_on_an_open_call` in `bridge/tests/test_telegram.py` also
replays `48df362`'s CallScope counterexample.

## The apps, the routed turn, Share and the calendar (2026-10-05)

Four models of the code at `a35b043` (origin/main): the direct apps and Composio, the toolset router's
turn, Share's file gate, and the calendar's writes and free slots. Each was written by one agent and
checked by an adversarial verifier against the Python line by line; the verifiers found five more
bugs and two places where a model was stronger than the code, and corrected the models. Every bug
is **fixed** on branch `feat/apps-mcp`. In each model `current_violates` is the code at `a35b043`, the
`Fix` (or `fix*`) definitions mirror the fixed lines, and `fixed_invariant` proves the property of the
fixed code for every input or trace. Each header gives the line numbers modelled and an "Applied" note.

| Model | Code | Property | What the counterexample was, and the fix |
|---|---|---|---|
| `Eve/AppsPolicy.lean` (38) | `apps_mcp.py` `tool_reads`, `name_reads`, `AppsBridge.decide` / `execute`, the bare-yes consent; `composio_tools.py` `decide`, `consequential_slugs`, `_personal_account_slugs`, `workbench_slugs`, `_allowed_toolkits`, `multi_account`, `ComposioBridge.start` | (P1) a direct app under "read" runs only tools that read; (P2) a writing call runs only by its policy or a bare yes; (P3) Composio never reaches the owner's personal Gmail/Calendar, and Drive/GitHub never join its session; (P4) a mixed call takes its strictest part's verdict | (1) P1: a tool with no `readOnlyHint` read by its name alone, so `find_and_replace` ran under Gmail's read policy. (2) P3: the alias "Personal" (any case, spaces) was not the personal account. (3) P3: a Gmail named in `EVE_COMPOSIO_TOOLKITS` joined with only the personal account, in a session that did not demand an account per call. (4) P3: `all` made a session with no limit, so Drive and GitHub joined. (5, verifier) P2/P3: a workbench script reaching `exec` through `__builtins__`, or unpickling a file, was judged "only computes" and ran unasked. **Fixed:** no hint is not a read; the alias is stripped and lowercased; Gmail/Calendar stay only with an active non-personal account; `all` sends `{"disable": [...]}`; `workbench_slugs` returns None for any name starting with "_", any pickle or HDF loader, `eval`/`exec`/`query` attributes and `operator.attrgetter`/`methodcaller`. P4 held from the start (`p4_*`). |
| `Eve/RoutedTurn.lean` (18) | `toolset_router.py` `route_turn`, `TurnToolsets.log_line`; `telegram.py` `_think`, `_turn_parts`, `parse_response`, `AllowedNames`, `_tool`, `_lendable`, `_lent_block`, `_refresh_lent`, `request`, INSTRUCTIONS, APPS_BLOCK_REST | (P1) no call fails a turn because of its name; (P2) a toolset's tool runs only when offered or auto-opened; (P3) a turn's open toolsets never carry over; (P4) the router's toolsets always include the keyword ones; (P5) every tool name the instructions write is offered | (1) P4: a router raising a BaseException left the box empty (KeyError), and a Route with unreadable toolsets raised TypeError: FAILED_LINE. (2) P1: a call with an empty name raised. (3) P1: the out-of-rounds summary parsed without `keep_unknown`. (4) P5: INSTRUCTIONS named the robot body's tools with EVE_ROBOT off. (5, verifier) P4: a Route whose `ms` is not a number failed at `log_line()`. (6, verifier) P5: APPS_BLOCK_REST named `web_search` with the search off. (7, verifier) P5: unrouted, `accounts_note` named COMPOSIO_MULTI_EXECUTE_TOOL while Composio's session was down. **Fixed:** `route_turn` reads the Route inside one `try`; blank names are unknown; the summary keeps unknown calls; INSTRUCTIONS has no robot paragraph unless `robot_enabled()` (INSTRUCTIONS_ROBOT); APPS_BLOCK_REST_NO_WEB; `accounts_note` only while `_composio_up()`. P2 and P3 held from the start (`current_p2_p3_invariant`, `start_forgets`). |
| `Eve/ShareGate.lean` (19) | `share.py` `resolve_file`, `_open_exact`, `fingerprint`, `stage_exact`, `Share.ask_and_share`; `telegram.py` `_share_tool`, `_ask_alone`, `_awaiting_answer`, `_show_question`, `_live_question` | (P3) a share is asked only with no other question live, and approved only by the answer to its own question; (P4) the bytes sent are the bytes the owner approved, of a file the owner-path rules allow | (1) P4: a folder swapped for a link to `~/.ssh` between the resolve and the open was followed (`O_NOFOLLOW` checks the last component only), so a hidden file was asked about under an innocent name and sent. (2, verifier, `slot_violates`) P3: the gate read the question slot, which lags a loop turn behind a settled question, so a share asked over a task's "Reply yes or no." and the task's yes sent the file. **Fixed:** `_open_exact` walks the real path one component at a time with `O_NOFOLLOW` and `dir_fd`; both gates ask `self._live_question() is not None`. |
| `Eve/CalendarEdits.lean` (23) | `google_rest.py` `_when`, `_event_body`, create/update/respond/delete, `_send_updates`, `_calendar_list_events`, `_list_one_calendar`, `free_slots`, `_calendar_suggest_time`, `is_place`, `place_links`, `compact_event` | (P1) an event's start/end never hold both date and dateTime; (P2) respond changes only the owner's own status; (P3) guests are emailed exactly as notifyGuests says; (P4) the merged list is every answered calendar's events in order, failing only when every read failed; (P5) free slots lie inside the owner's working hours, overlap no busy time; (P6) a Maps link exactly when the location is a place | (1) P4: an httpx error on the calendar list escaped `list_events` without reading the primary. (2) P5: each day's 9-to-18 window was built in the start's fixed offset, so after DST ended the hour before the working day was offered. (3, verifier) P5: the same root cause with a UTC start offered 02:00-11:00 in Los Angeles. **Fixed:** the list step catches `httpx.HTTPError` too; `free_slots` builds each day's window in the Mac's zone, per day. P1, P2, P3 and P6 held from the start (proved of the fixed code, which is the same there). |

Documented deviations (each model's header lists them in full):

- **AppsPolicy:** a tool name is its word list (`_words` is not modelled; camelCase is one word); the
  fixed workbench judge is assumed sound, since a denylist over Python's AST cannot be proved complete
  (`fixJudge`); a multi-execute `tools` that is not a list is read as no items and forwarded by the SDK
  as it is; a bundle toolkit (Composio's `googlesuper`) carrying Gmail actions under another prefix is
  outside P3; that the session holds only Composio's meta tools is the SDK's behaviour.
- **RoutedTurn:** P5 is not proved for a rundown (`rundown_prompt_gap`); the memory headers' tool names
  are not modelled; a call whose arguments are not JSON, or with no call_id, still fails a turn (outside
  P1); Composio's tool list is assumed to hold every meta name the blocks write; `web_search` is assumed
  absent from INSTRUCTIONS (`hweb`, checked by a regex).
- **ShareGate:** P4 holds up to the staged copy's path (another owner process could rewrite it before
  Messages, Gmail or Drive reads it); hard links are not modelled; typed-answer routing to the newest question
  is assumed from `Questions.lean`. `chief_ask` still gates on the slot (not modelled).
- **CalendarEdits:** times are minutes and the zone a function; days are an arbitrary list; P5 allows
  zero or negative durations (invalid input); `free_slots` sorts busy by (start, end), the model by start
  only (P5 holds for every start-sorted order); an httpx error on every calendar escapes `call_tool`
  as an exception, which the model counts as a failure like any other.

Replays, each failing on `a35b043` and passing on the fix: `bridge/tests/test_lean_appspolicy_replay.py`
(5, plus 3 controls), `test_lean_routedturn_replay.py` (9), `test_lean_sharegate_replay.py` (2) and
`test_lean_calendaredits_replay.py` (3).

## A "new claude" joining late (2026-10-06)

One model of the code at `5f00a53` (origin/main). `current_violates` models `_launch_step` and the relay switches
at `5f00a53`; `fixed_invariant` proves the properties of the fixed code for every trace.

| Model | Code | Property | What the counterexample was, and the fix |
|---|---|---|---|
| `Eve/RelayJoin.lean` (7) | `telegram.py` `_launch_step`, `_relay_to`, the CLAUDE_ON/CLAUDE_OFF and `codex` branches of `_handle` | (P1) the Claude and Codex relays are never both on; (P2) after "claude off" the Claude relay stays off until the next "claude on" | `_launch_step` took `_join_after_launch`, awaited the terminal for seconds, then joined without looking again. (1) `codex <folder>` in that window left Claude and Codex both on. (2, `current_violates_off`) "claude off" in that window was undone when the terminal opened. **Fixed:** the late join happens only if `_claude_epoch` is unchanged since the launch began, and the Codex switch moves the epoch (the CLAUDE_ON/OFF branch already did). `fixed_still_joins`: with no switch in the window, the join still happens. |

Deviations: each relay is one Bool for one owner chat (a second owner chat is refused by the Codex branch and
not modelled); which words make which switch is left out; the epoch is a Bool "moved since the launch began",
not a counter.

Replays, each failing on `5f00a53` and passing on the fix: `bridge/tests/test_telegram.py`
`test_a_codex_folder_while_a_new_claude_opens_cancels_its_late_join` and
`test_claude_off_while_a_new_claude_opens_cancels_its_late_join`, with the control
`test_with_no_switch_the_late_join_still_happens`.

### The models caught up with the Python (2026-10-09)

An audit of the day's commits (`e7333ae..712dd34`: Gmail's policy to "ask", an email asked whole, the Google
Contacts toolset, Gmail attachments in `share_file`) against the thirty models found no theorem made false, and
three places where a model had fallen short of the code. Each is a model update, with no bug and no replay:

- **AppsPolicy**: `_direct_tool` now asks an email (`apps_mcp.WHOLE_ASKS`) whole, and refuses it unasked
  (`tool_gate.TOO_LONG`) when the whole preview is over 3,500 characters, so the model's "a bare yes runs it" was
  wider than the code. `execDirect` takes an `askable` bit, `exec_direct_gate` and P2 require it, and
  `too_long_never_executes` says an email too long to show runs on no answer at all, even "yes" (the control is
  the same call with a question that fits). `exec_only_offered` covers the two callers that reach
  `AppsBridge.execute` with no `decide`, `_load_repos` and the notetaker's calendar read: under a read policy
  they can run only a hinted, verb-free tool. The header's line numbers are those of `712dd34`.
- **RoutedTurn**: an eighth toolset, Contacts, is keyword-only (`toolset_router.KEYWORD_ONLY`): its words open
  it and `route_turn` masks Jev's answer to `TOOLSETS`. `Fix.routeTurn` carries the mask, and
  `fixed_route_keyword_only` proves a keyword-only toolset is open exactly when the words named it, for every
  answer; `jev_adds_routed_toolsets` is the control. `Kit.direct` now says what it mirrors, membership in
  `AppsBridge.apps`, since the router's `DIRECT` constant is unused.
- **ShareGate**: `_send_gmail` is a third sink of the staged copy (`stage_exact`, then `staged.read_bytes()`),
  so P4 holds for a Gmail attachment as it does for Messages and Drive; the docstring names it. The channel bit
  (`Pending.attach`, frozen at the question) is carried by field, not modelled; `test_share.py` pins it.

## eve lite (2026-10-13)

Three models for [eve lite](lite.md), the one-click variant ([its spec](plans/eve-lite.md)), written beside its code
on branch `feat/one-click-variant`. The code was not committed while it was modelled, so the line numbers in the docstrings
are those of that working tree. WebPairing is new and has the third shape, `code_invariant`; the other two gain a
section each.

| Model | Code | Property | What it found |
|---|---|---|---|
| `Eve/WebPairing.lean` (24, new) | `web_auth.py` `PairingWindow`, `MacLogin`, `Devices`; the composition the spec fixes for `lite_web.py` (a device is added only after a successful pair or Mac login, and revoke closes the device's live peer through `RtcHub.close_device`) | (P1) a pairing window pairs one device, and a login code logs in once; (P2) a window pairs only within 5 minutes, a login code only within 2, and a cookie checks only while its device was seen within 400 days; (P3) at most 5 tries ever reach one window, the one that pairs included; (P4) a revoked device is out of the store and has no live peer; (P5) a cookie checks only as an unrevoked device that a success minted, by its token's hash | Right from the start. `naive_violates` takes each mechanism out alone and breaks its property. Stated, not a bug: a device that `_kept` forgets on a write (unseen for 400 days, or past 64 devices) stops checking, but its live peer stays until it hangs up, because only revoke closes peers (`forget_is_not_revoke`). A revoke whose write fails is the same case: its rows leave memory and it raises before any peer is closed. Device ids are random, so a new device can draw an id that was revoked; the model tells devices apart by the mint that made them. |
| `Eve/AppsPolicy.lean` (+20) | `composio_tools.py` `decide(..., personal_direct=True)`, `decide_lite`, `manage_needs_yes`; `telegram.py` `_app_tool` | `personal_direct=True` is today's `decide` for every call. `False` changes only a call that reaches the personal account, and only towards run. P4 and P1 hold for both values. In lite, every write that is not routine, every code tool, every call `decide` cannot read and every connection change that `manage_needs_yes` does not prove runs only after a whole question and a bare yes (P2) | No bug. P3 is given up on purpose in lite, where the owner's own Gmail is the only Gmail (`lite_controls`). |
| `Eve/RoutedTurn.lean` (+5) | `telegram.py` in lite: `request`, `own_tools`, `tools_for` (through `profile.filter_tools`), `apps_block_lite`, `graded` | (P5) every tool name a lite turn's instructions write is offered by that turn, with no assumption on what Composio lists (`lite_invariant`) | P5 does not hold for an app turn of grade "instant", in full eve or in lite: `graded` drops think_hard from the tools, while the instructions and `config.about` still name it (`instant_prompt_gap`, `lite_instant_gap`). A probe of the Python confirmed both. Not fixed here, because full eve does not change in this work. |

Deviations, as each model's docstring states them: WebPairing has one clock that only moves on (the code reads the
monotonic clock for windows and codes, the wall clock for devices); `forget` may drop any devices, where `_kept` drops
the ones seen longest ago; the routing that only the Mac's own page can open a window is `lite_web.py`'s, tested
there. In AppsPolicy, the workbench judge is the fixed one (`fixJudge`'s contract). In RoutedTurn, the text that rides
every lite turn names only lite tools (`hbase`); `test_lite_brain.py` holds the Python to that.

## Running it

```bash
# toolchain (once): elan with Lean 4.34.1, pinned in verification/lean-toolchain
curl -sSfL https://raw.githubusercontent.com/leanprover/elan/master/elan-init.sh | sh -s -- -y --no-modify-path

cd verification
~/.elan/bin/lake build          # compiles every model
node check-all.mjs              # every theorem: no sorry, no native_decide, standard axioms only
```

`check.mjs` is the honest gate. It fails on any `sorry`. It then asks Lean which axioms each theorem
rests on, and refuses anything outside `propext`, `Classical.choice` and `Quot.sound`. So a proof
closed by `native_decide`, which trusts compiled code, is refused. The models use core Lean only,
with no Mathlib.

## What a proof here does and does not say

A theorem is about the **model**, not the Python. Two things connect them:

- **Every transition cites the Python lines it models.** A reviewer can check the correspondence
  line by line.
- **The replay tests run the counterexample against the real code.** For the command classifier,
  `test_is_simple_agrees_with_the_lean_model` also runs the Lean `simple` and the Python `is_simple`
  on 3,000 random commands and requires them to agree on each one. Its positive control, dropping
  `#` from the Lean model, makes it fail.

Stated assumptions, per model:

- **Command.** The shell reads the characters in `shellMeta`, and whitespace other than a space,
  as structure. The runner list and the run-or-write flag table cover the programs an owner would
  allow, and the theorem is only as strong as that table. A command the guard refuses falls
  through as unmatched: to Claude Code's own flow, or to "ask" under `strict`.
- **Questions.** The done-callback that moves the slot is modelled as running when the future
  settles. asyncio actually runs it one loop step later.
- **TaskStop.** A cancel before the agent returns makes it return its stopped result, as the
  agent contract says.
- **Forget.**
  - Each step is atomic under its lock, and model output contains only lines from its input.
  - Anything said after the forget's redaction step counts as new input.
  - The records generation works across processes (`records/.forgets` under the folder flock).
    The mem0 one is in-process, relying on the local store allowing one process at a time.
- **Serial.**
  - (B) is proved for every flap cycle of the form "resync acks, at most one answered poll,
    then deaf". The rule that only two consecutive answered polls de-escalate is proved for
    every single step.
  - (C) treats request ids as distinct. On the wire a chunk is identified by (type, bytes
    written to the file so far), and other ack types still match by type.
- **Voice.** The model covers captions mode, the default.
- **Chief.** One card at a time is modelled; cards share only the Desk and the lesson store,
  which are modelled whole. A card runs one phase at a time. The door floor (the verb
  regex), the choice of executor, the Codex floor for acts and the one Mac slot are outside
  the model; they are unit-tested. A door only rises, and only before the phase first ran.
  A reflection that errors is no event: no lesson, no retry. Money and wall time are two
  whole-number counters.
