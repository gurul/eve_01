/-
  Eve.Voice — the voice session's face state and its question slot.

  Models bridge/src/eve_bridge/voice_agent.py (line numbers are the post-fix file).

  Sub-target A: once nothing is in flight (no backend response, no requested one), the caption pager is
  idle and no assistant turn is open, the face is not `speaking` or `thinking`.
  Scope: captions output mode (the default, `VoiceConfig.output == "captions"`), where
  `session.output_audio.delta` plays nothing (voice_agent.py:1313-1317), so no audio is ever playing.
  Audio mode is NOT modelled.

  Finding: the survey's "deferred state is discarded" (`_flush_pager`, :1199-1206) is by design and
  harmless on its own — the completion branch (:1350-1368) re-derives the phase. The real defect was a
  backend stream `error` (formerly its own `elif`, never clearing `_response_active`): the flag stayed
  set with nothing in flight, so the face held `thinking` and `_request_response` (:1980-1985) deferred.
  The fix treats `error` as terminal, like `response.failed` (:1350).

  Sub-target B: `_ask_user` (:1940-1957) must not leave its own future in `_pending_answer` when it
  returns or raises. The note and the spoken question used to run before the `try`.
-/
namespace Eve.Voice

/-! ## Sub-target A: the face -/

inductive Face where
  | listening | speaking | thinking | working
  deriving DecidableEq, Repr

/-- The phases the invariant forbids once everything is settled. -/
def bad : Face → Bool
  | .speaking => true
  | .thinking => true
  | _ => false

structure S where
  face : Face          -- self.state (:824-830)
  active : Bool        -- self._response_active (:815)
  wanted : Bool        -- self._response_wanted (:816)
  deferred : Bool      -- self._state_after_captions is not None (:800); no task runs, so it is "listening"
  busy : Bool          -- self._pager.busy (caption_pager.py:168)
  replyOpen : Bool     -- self._reply_open (:819)
  turnOpen : Bool      -- self._turns.speaker == "assistant" (text is non-empty whenever it is)
  inflight : Bool      -- ghost: a backend response is really running
  requested : Bool     -- ghost: a response.create was sent and response.created has not come yet
  deriving DecidableEq, Repr

def s0 : S :=
  { face := .listening, active := false, wanted := false, deferred := false, busy := false,
    replyOpen := false, turnOpen := false, inflight := false, requested := false }

inductive Ev where
  | delegation      -- session.delegation.created (:1318-1326)
  | created         -- response.created (:1340-1341)
  | done            -- response.completed / failed / incomplete (:1350-1368)
  | error           -- backend stream `error` (current: log only; fixed: :1350)
  | request         -- _request_response after a tool result (:1980-1985)
  | assistantDelta  -- session.output_transcript.delta (:1399-1412)
  | closeTurn       -- _close_due_turn → _close_turn "assistant" (:1419-1451)
  | pageClear       -- a caption tick: the held page clears, then _flush_pager (:1199-1206, :1218)
  | userDelta       -- session.input_transcript.delta: barge-in (:1379-1398)
  | audioDelta      -- session.output_audio.delta: plays nothing in captions mode (:1313-1317)
  deriving DecidableEq, Repr

/-- `_set_after_captions` (:1183-1188). -/
def setAfter (s : S) : S :=
  if s.busy then { s with deferred := true } else { s with face := .listening }

/-- `_flush_pager` (:1199-1206): pops the deferred phase once the pager is idle, drops it while a
response is active (the completion branch sets the phase again). -/
def flush (s : S) : S :=
  if !s.busy && s.deferred then
    if s.active then { s with deferred := false } else { s with deferred := false, face := .listening }
  else s

/-- The terminal branch (:1350-1368). -/
def terminal (s : S) : S :=
  let s := { s with active := false, inflight := false }
  if s.wanted then { s with wanted := false, requested := true }       -- :1359-1361 response.create sent
  else if s.turnOpen then s                                              -- :1362 speaker is assistant
  else setAfter (if s.replyOpen then { s with busy := false, replyOpen := false } else s)  -- :1365-1368

/-- The assistant turn closes: final page up and held, then `_set_after_captions` (:1419-1451). -/
def closeTurn (s : S) : S :=
  if s.turnOpen then setAfter { s with turnOpen := false, replyOpen := false, busy := true } else s

/-- One event. `fixed = false` is the code before the fix, `true` after. -/
def step (fixed : Bool) (s : S) : Ev → S
  | .delegation =>
      let s := { s with active := true, inflight := true }
      if s.busy then s else { s with replyOpen := true, face := .thinking }
  | .created => { s with active := true, inflight := true, requested := false }
  | .done => terminal s
  | .error => if fixed then terminal s else { s with inflight := false }  -- the stream ended; flag stays
  | .request => if s.active then { s with wanted := true } else { s with requested := true }
  | .assistantDelta => { s with turnOpen := true, face := .speaking, replyOpen := true, busy := true }
  | .closeTurn => closeTurn s
  | .pageClear => flush { s with busy := false }
  | .userDelta =>
      let s1 := closeTurn s
      let s2 := if s1.replyOpen || s1.busy
        then { s1 with busy := false, replyOpen := false, deferred := false } else s1
      { s2 with face := .listening, turnOpen := false }
  | .audioDelta => s

def run (fixed : Bool) (es : List Ev) : S := es.foldl (step fixed) s0

/-- Nothing in flight, pager idle, no assistant turn open (captions mode: no audio plays). -/
def settled (s : S) : Bool := !s.inflight && !s.requested && !s.busy && !s.turnOpen

/-- Current code: a delegation followed by a backend `error` leaves the face on `thinking`
with nothing in flight. -/
theorem A_current_violates :
    ∃ es : List Ev, settled (run false es) = true ∧ bad (run false es).face = true :=
  ⟨[.delegation, .error], by decide⟩

/-- The strengthened invariant of the fixed machine: the flag tracks the real response, and a
`speaking`/`thinking` face always has something that will move it on. -/
def Inv (s : S) : Bool :=
  (s.active == s.inflight) &&
  (!bad s.face || s.inflight || s.requested || s.turnOpen || (s.deferred && s.busy))

theorem inv_s0 : Inv s0 = true := by decide

theorem inv_step (s : S) (e : Ev) (h : Inv s = true) : Inv (step true s e) = true := by
  obtain ⟨face, active, wanted, deferred, busy, replyOpen, turnOpen, inflight, requested⟩ := s
  revert h
  cases e <;> cases face <;> cases active <;> cases inflight <;> cases busy <;> cases deferred <;>
    cases turnOpen <;> cases requested <;> cases wanted <;> cases replyOpen <;> decide

theorem inv_run (es : List Ev) : ∀ s, Inv s = true → Inv (es.foldl (step true) s) = true := by
  induction es with
  | nil => intro s h; exact h
  | cons e es ih => intro s h; exact ih _ (inv_step s e h)

theorem inv_settled (s : S) (h : Inv s = true) (hs : settled s = true) : bad s.face = false := by
  obtain ⟨face, active, wanted, deferred, busy, replyOpen, turnOpen, inflight, requested⟩ := s
  revert h hs
  cases face <;> cases inflight <;> cases busy <;> cases deferred <;> cases turnOpen <;>
    cases requested <;> cases active <;> cases wanted <;> cases replyOpen <;> decide

/-- Fixed code: on every trace, a settled session shows neither `speaking` nor `thinking`. -/
theorem A_fixed_invariant :
    ∀ es : List Ev, settled (run true es) = true → bad (run true es).face = false :=
  fun es hs => inv_settled _ (inv_run es s0 inv_s0) hs

/-! ## Sub-target B: the question slot -/

/-- How the one `wait_for` ends: an answer, the 60 s timeout, or a cancellation (the task is torn down). -/
inductive Wait where
  | answer | timeout | cancelled
  deriving DecidableEq, Repr

/-- One run of `_ask_user`: whether the owner already said goodbye, whether `_backend_note` and
`_speak` succeed, and how the wait ends. -/
structure Ask where
  farewell : Bool
  noteOk : Bool
  speakOk : Bool
  wait : Wait
  deriving DecidableEq, Repr

inductive Outcome where
  | returned | raised
  deriving DecidableEq, Repr

/-- Current `_ask_user`: the future is stored (:1944), then the note and the question are sent
before the `try`, so a raise there skips the `finally`. `p` is `_pending_answer` on entry, `fut` the
id of the future this call creates. Result: how the call ends and `_pending_answer` afterwards. -/
def askCurrent (a : Ask) (p : Option Nat) (fut : Nat) : Outcome × Option Nat :=
  if a.farewell then (.returned, p)                 -- early return before the future exists
  else
    let p := some fut                                -- self._pending_answer = loop.create_future()
    if !a.noteOk then (.raised, p)                   -- await self._backend_note(...) raises
    else if !a.speakOk then (.raised, p)             -- await self._speak(...) raises
    else match a.wait with                           -- try: ... finally: self._pending_answer = None
      | .answer => (.returned, none)
      | .timeout => (.returned, none)
      | .cancelled => (.raised, none)

/-- Fixed `_ask_user` (:1940-1957): the `try` opens right after the future is stored, so the
`finally` covers the note and the question too. -/
def askFixed (a : Ask) (p : Option Nat) (_fut : Nat) : Outcome × Option Nat :=
  if a.farewell then (.returned, p)
  else
    let r : Outcome :=
      if !a.noteOk then .raised
      else if !a.speakOk then .raised
      else match a.wait with
        | .answer => .returned
        | .timeout => .returned
        | .cancelled => .raised
    (r, none)                                        -- finally: self._pending_answer = None

/-- Current code: the note fails, `_ask_user` raises, and the slot still holds its future. -/
theorem B_current_violates :
    ∃ a : Ask, askCurrent a none 0 = (.raised, some 0) :=
  ⟨{ farewell := false, noteOk := false, speakOk := true, wait := .answer }, by decide⟩

/-- Fixed code: on every path, returned or raised, the slot does not hold this call's future. -/
theorem B_fixed_invariant :
    ∀ (a : Ask) (p : Option Nat) (fut : Nat), p ≠ some fut → (askFixed a p fut).2 ≠ some fut := by
  intro a p fut hp
  unfold askFixed
  cases a.farewell <;> simp [hp]

theorem current_violates :
    (∃ es : List Ev, settled (run false es) = true ∧ bad (run false es).face = true) ∧
    (∃ a : Ask, askCurrent a none 0 = (.raised, some 0)) :=
  ⟨A_current_violates, B_current_violates⟩

theorem fixed_invariant :
    (∀ es : List Ev, settled (run true es) = true → bad (run true es).face = false) ∧
    (∀ (a : Ask) (p : Option Nat) (fut : Nat), p ≠ some fut → (askFixed a p fut).2 ≠ some fut) :=
  ⟨A_fixed_invariant, B_fixed_invariant⟩

end Eve.Voice
