/-
  Eve.TalkIntent — the "Talk to eve" App Intent behind the Action Button, the Control, Siri and the Live
  Activity's button, and the Live Activity that an AudioRecordingIntent must keep up while it records.

  Models:
  - ios/EveLink/Intents/TalkIntent.swift: `TalkIntent.perform`, with its `catch is TalkNeedsForeground` branch
    (lines 35-51);
  - ios/EveLink/App/PhoneLink.swift: `PhoneLink.press` (21-35), `PhoneLink.pressAfterIntent` (40-50),
    `TalkActivity.ensureUp` (81-85) and `TalkActivity.show` (87-113, the request path);
  - ios/EveLink/Shared/Relay.swift: `pressWouldStart` (331-336), `press` (340-350), `phoneBegan` (382-404), and
    the `phase` didSet that calls `onPhase` = `TalkActivity.show` (63).
  Line numbers are those of the files at 2ad3550 (branch verify/lean-2026-10-04). The old code is commit 48df362
  (`git show 48df362:ios/EveLink/...`). The LinkTurn fixes (branch fix/lean-findings, 2026-10-04) moved Relay.swift's
  lines but changed nothing this model reads: `pressWouldStart` is now 337-341, `press` 346-356, `phoneBegan`
  395-417 (`phase = .listening` 407, `phone.start` 410), `phoneEnded` 472; in `phoneBegan` only
  `lastEndsItself = endsItself` became `hold.began(endsItself:)` (398). TalkIntent.swift and PhoneLink.swift are
  unchanged. So the model, and `fixed_invariant`, stand for the fixed code as they are.

  Apple's rule (AudioRecordingIntent): an intent may record only while a Live Activity of the app is live. When
  the intent's `perform` started the microphone with none, App Intents trapped and the app "quit unexpectedly"
  (2026-10-04, two crash logs).

  The properties, over every trace of events:
  - (P1) whenever the intent's `perform` starts the microphone, a Live Activity is live at that moment;
  - (P2) a press while paired and permitted that would start a turn (`pressWouldStart`) is never dropped: when the
    press is over (`pc = idle`) the turn has started, and the only other way out of `perform` is the foreground
    continuation, where the app is in front and its own press is pending (`pc = appPress → front`).

  Finding (the old code, `current_violates`). In 48df362 `perform` was `try await PhoneLink.press()`, which went
  straight to `relay.press()` → `phoneBegan`. There `phase = .listening` (Relay.swift:304 old) ran
  `TalkActivity.show`, whose `guard ActivityAuthorizationInfo().areActivitiesEnabled, let activity = try?
  Activity.request(...) else { return }` (PhoneLink.swift:47-49 old) returned quietly on a refusal, and
  `phoneBegan` went on to `phone.start` (Relay.swift:307 old): the microphone started inside the intent with no
  Live Activity. iOS refuses a Live Activity started from the background in some states, and always when Live
  Activities are off for the app, so both `[fireBg, reqFail, micStart]` and `[fireFg, reqFail, micStart]` trap.
  (A `phase` already `.listening` skipped `show` altogether, with the same result; `reqFail` stands for it too.)

  The current code (`fixed_invariant`). `PhoneLink.press` asks `TalkActivity.ensureUp()` before `relay.press()`
  whenever `pressWouldStart`, and throws `TalkNeedsForeground` when it is not up. `perform` then continues in the
  foreground (`continueInForeground`), asks `ensureUp()` again, and presses only if it is up; otherwise it returns
  and leaves the press to the app in front (`pressAfterIntent`, 400 ms later, outside the intent). No bug was found
  in the current code for P1 or P2.

  Abstractions, stated:
  - Paired and permitted: `pairing != nil`, `phone != nil`, `micAllowed == true`, and `phone.start` does not
    throw. (Their failures throw a `TalkFailure` the intent shows; they are not the "silently nothing" of P2.)
  - iOS 26 or later: `#available(iOS 26.0, *)` holds, so `TalkNeedsForeground` always continues in the foreground,
    and `continueInForeground` returns (the owner does not refuse to unlock; that throws, and is shown).
  - One press at a time: a fire while a press is still being handled (`pc ≠ idle`) is not modelled.
  - Atomicity. Everything from the fire to `phone.start` is synchronous on the main actor except two awaits:
    `continueInForeground` (state `needFg`) and the 400 ms sleep of `pressAfterIntent` (state `appPress`).
    `Activity.request` is synchronous; its answer is the event `reqOk` / `reqFail`, and the step to the microphone
    is the event `micStart`. Between them no other event can run, so `dismiss` and `turnEnd` change nothing in the
    states `request`, `request2` and `starting`.
  - `reqOk` / `reqFail` are free: the theorem holds whatever iOS answers, in the background or the foreground, with
    Live Activities on or off.
  - `pressWouldStart` and `press` compute the same `PressAction.decide` from the same state with no await between
    them (PhoneLink.swift:26-30), so "would start" is "not talking": `eveSpeaking` (barge-in) is a start too.
  - Outside the model: the app leaving the front during `pressAfterIntent`'s 400 ms sleep. Its press then runs in
    the background, outside any intent (so no trap), and a failure there is only logged.
-/
namespace Eve.TalkIntent

/-- Where the press is. -/
inductive Pc where
  | idle        -- no press being handled
  | request     -- the first `ensureUp()` (current) / `show(.listening)` (old) is calling `Activity.request`
  | needFg      -- `TalkNeedsForeground` was thrown; `perform` awaits `continueInForeground`
  | request2    -- in the foreground, the second `ensureUp()` is calling `Activity.request`
  | starting    -- inside `perform`, `relay.press()` → `phoneBegan` is about to call `phone.start`
  | appPress    -- `perform` returned; `pressAfterIntent`'s task will call `relay.press()` in the app
  deriving DecidableEq, Repr

inductive Event where
  | fireBg      -- the intent fires with the app in the background (the Action Button, a locked phone)
  | fireFg      -- the intent fires with the app in front
  | reqOk       -- `Activity.request` returns an activity
  | reqFail     -- `Activity.request` throws (Live Activities off, or refused from the background)
  | fgDone      -- `continueInForeground` returns: the app is in front
  | micStart    -- the code reaches `phone.start` (Relay.swift:397)
  | dismiss     -- the Live Activity ends (swiped away, or the call's phase went `.ended`)
  | turnEnd     -- the phone's turn ends (its pause, or the call ended)
  deriving DecidableEq, Repr

structure S where
  pc : Pc := .idle
  live : Bool := false         -- `TalkActivity.isLive(current)`
  front : Bool := false        -- the app is in front
  talking : Bool := false      -- `Relay.phoneTalking` (`turn == .phone`)
  wantStart : Bool := false    -- ghost: a press that would start a turn is not yet served
  badMic : Bool := false       -- ghost: the intent's `perform` started the mic with no Live Activity
  deriving DecidableEq, Repr

/-- P1 and P2. -/
def Spec (s : S) : Bool :=
  !s.badMic &&
  (s.pc != .idle || !s.wantStart) &&
  (s.pc != .appPress || s.front)

/-- `phone.start` and the turn it opens (Relay.swift:394-404); `inPerform` says whether the intent is still
running. -/
def startMic (s : S) (inPerform : Bool) : S :=
  { s with badMic := s.badMic || (inPerform && !s.live), talking := true, wantStart := false, pc := .idle }

/-- The press's `.send` (Relay.swift:347 → `phoneEnded`, 453-462): the turn goes, no microphone starts. -/
def send (s : S) : S := { s with talking := false }

/-- Events that need an await to run: none can land inside a synchronous stretch. -/
def awaitable (s : S) : Bool := s.pc == .idle || s.pc == .needFg || s.pc == .appPress

/-! ## The old code (48df362) -/

def Cur.step (s : S) : Event → S
  -- TalkIntent.swift:29-34 old: `try await PhoneLink.press()` → `relay.press()` (Relay.swift:259-268 old).
  | .fireBg | .fireFg =>
      if s.pc != .idle then s
      else
        let s := { s with front := false }
        if s.talking then send s
        -- `.start` / `.bargeIn` → `phoneBegan` → `phase = .listening` (304 old) → `show` (PhoneLink.swift:41-53 old):
        -- an activity already live is updated; otherwise `Activity.request` (47-49 old).
        else
          let s := { s with wantStart := true }
          if s.live then { s with pc := .starting } else { s with pc := .request }
  -- PhoneLink.swift:50-52 old: `current = activity.id`.
  | .reqOk => if s.pc == .request then { s with live := true, pc := .starting } else s
  -- PhoneLink.swift:47-49 old: `else { return }`; `phoneBegan` goes on to `phone.start` regardless.
  | .reqFail => if s.pc == .request then { s with pc := .starting } else s
  | .fgDone => s                                              -- the old intent never continued in the foreground
  -- Relay.swift:305-312 old: `phone.start`, inside `perform`.
  | .micStart => if s.pc == .starting then startMic s true else s
  | .dismiss => if awaitable s then { s with live := false } else s
  | .turnEnd => if awaitable s then { s with talking := false } else s

/-- `fireFg` differs from `fireBg` only in `front`; the old code never read it. -/
def Cur.step' (s : S) (e : Event) : S :=
  let t := Cur.step s e
  if e == .fireFg && s.pc == .idle then { t with front := true } else t

def Cur.run (evs : List Event) : S := evs.foldl Cur.step' {}

/-- The crash, twice: fired from the background, iOS refuses the Live Activity and the intent starts the mic
anyway; fired with the app open but Live Activities off for Eve Link, the same. -/
theorem current_violates :
    let a := Cur.run [.fireBg, .reqFail, .micStart]
    let b := Cur.run [.fireFg, .reqFail, .micStart]
    a.badMic = true ∧ a.live = false ∧ Spec a = false ∧
    b.badMic = true ∧ b.front = true ∧ Spec b = false := by
  decide

/-! ## The current code -/

def Fix.step (s : S) : Event → S
  -- TalkIntent.swift:36 → PhoneLink.press (21-35). `pressWouldStart` (Relay.swift:331-336) false: `relay.press()`
  -- sends (347). True: `ensureUp()` (PhoneLink.swift:81-85): live → true at once; else `show(.listening)` requests.
  | .fireBg | .fireFg =>
      if s.pc != .idle then s
      else
        let s := { s with front := false }
        if s.talking then send s
        else
          let s := { s with wantStart := true }
          if s.live then { s with pc := .starting } else { s with pc := .request }
  -- PhoneLink.swift:104, 110: `current = activity.id`; `ensureUp` returns true. From `request`, PhoneLink.swift:29-30
  -- `relay.press()`; from `request2`, TalkIntent.swift:44-45 `PhoneLink.press()` again (now `ensureUp` is true at
  -- once) → `relay.press()`.
  | .reqOk => if s.pc == .request || s.pc == .request2 then { s with live := true, pc := .starting } else s
  -- PhoneLink.swift:105-107: `show` returns, `ensureUp` is false. From `request`: PhoneLink.swift:26-27 throws
  -- `TalkNeedsForeground` → TalkIntent.swift:37-42 `continueInForeground`. From `request2`: TalkIntent.swift:46-50,
  -- `pressAfterIntent()` and `perform` returns.
  | .reqFail =>
      if s.pc == .request then { s with pc := .needFg }
      else if s.pc == .request2 then { s with pc := .appPress }
      else s
  -- TalkIntent.swift:42-44: in front; `ensureUp()`: live → press, else request.
  | .fgDone =>
      if s.pc == .needFg then
        let s := { s with front := true }
        if s.live then { s with pc := .starting } else { s with pc := .request2 }
      else s
  -- Relay.swift:394-404, `phone.start`: inside `perform` from `starting`; from `appPress` it is
  -- PhoneLink.swift:42-45, `relay.press()` after the 400 ms sleep, outside the intent.
  | .micStart =>
      if s.pc == .starting then startMic s true
      else if s.pc == .appPress then startMic s false
      else s
  | .dismiss => if awaitable s then { s with live := false } else s
  | .turnEnd => if awaitable s then { s with talking := false } else s

def Fix.step' (s : S) (e : Event) : S :=
  let t := Fix.step s e
  if e == .fireFg && s.pc == .idle then { t with front := true } else t

def Fix.run (evs : List Event) : S := evs.foldl Fix.step' {}

/-! ### Proof: an inductive invariant, checked on every state and event. -/

/-- The spec, plus: the mic is only reached inside `perform` with the activity live, and the second request is
made with the app in front. -/
def Inv (s : S) : Bool :=
  Spec s && (s.pc != .starting || s.live) && (s.pc != .request2 || s.front)

theorem inv_step : ∀ (s : S) (e : Event), Inv s = true → Inv (Fix.step' s e) = true := by
  intro s e
  rcases s with ⟨pc, a, b, c, d, f⟩
  cases pc <;> cases a <;> cases b <;> cases c <;> cases d <;> cases f <;> cases e <;> decide

theorem inv_run (evs : List Event) : ∀ s : S, Inv s = true → Inv (evs.foldl Fix.step' s) = true := by
  induction evs with
  | nil => intro s h; exact h
  | cons e rest ih => intro s h; exact ih _ (inv_step s e h)

/-- For every trace of fires (background or front), Activity requests that succeed or fail, foreground
continuations, mic starts, dismissals and turn ends: the intent never starts the microphone without a live
Live Activity (P1), and a press that would start a turn is never dropped: it has started the turn by the time the
press is over, or it was handed to the app in front (P2). -/
theorem fixed_invariant (evs : List Event) : Spec (Fix.run evs) = true := by
  have h := inv_run evs {} (by decide)
  have spec_of_inv : ∀ s : S, Inv s = true → Spec s = true := by
    intro s; unfold Inv; cases Spec s <;> simp
  exact spec_of_inv _ h

/-- The traces that crashed the old code, on the current code: from the background the intent continues in the
foreground and starts the turn there with the activity up; with Live Activities off the app starts the turn after
the intent returned, never inside it. -/
theorem fixed_replays_counterexample :
    let a := Fix.run [.fireBg, .reqFail, .fgDone, .reqOk, .micStart]
    let b := Fix.run [.fireFg, .reqFail, .fgDone, .reqFail]
    let c := Fix.run [.fireFg, .reqFail, .fgDone, .reqFail, .micStart]
    a.talking = true ∧ a.badMic = false ∧ a.live = true ∧
    b.pc = .appPress ∧ b.front = true ∧
    c.talking = true ∧ c.badMic = false ∧ Spec c = true := by
  decide

end Eve.TalkIntent
