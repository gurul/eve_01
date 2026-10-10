/-
  The OpenAI seam's retry on a TLS drop (bridge/src/eve_bridge/computer_agent.py,
  `make_response_creator` and `make_stream_creator`).

  On 2026-09-28 three phone-call turns died on a bare `ssl.SSLError`: a TLS read that fails after the
  handshake comes out of anyio unmapped, and the SDK neither wraps nor retries it. One drop killed the
  turn. The obvious cure, "always retry", is wrong on the streamed call: a sentence already read out to
  the owner would be read out again. On 2026-09-30 one retry was not enough either: it went out 120 ms
  later on the same connection pool and dropped again. The code now retries twice, each time after a
  pause and on a new client (the new-client part is tested in bridge/tests/test_response_creators.py;
  this model is about which tries happen). The properties:

    (1) a turn fails only because the connection dropped on all three tries, or dropped after text was
        read out;
    (2) no text is read out twice.

  Abstraction. `drop` is `ssl.SSLError` raised by the call; `text` is a delta handed to `on_text` (read
  out at once by the call's SentenceStream); `done` is the response completing. `attempt` is which try is
  running (a try starts only after a drop). `spokePrior` is text having been read out during an earlier
  try. `duplicated` is text read out in a later try after text was read out in an earlier one (the same
  reply, twice). `droppedEvery` (the drop that failed the turn came on the third try) and
  `droppedAfterText` are why a failure is allowed.
-/
namespace Eve.ResponseRetry

inductive Attempt where
  | first
  | second
  | third
  deriving DecidableEq, Repr

inductive Event where
  | drop
  | text
  | done
  deriving DecidableEq, Repr

structure S where
  attempt : Attempt := .first
  spoke : Bool := false            -- any text read out so far
  spokePrior : Bool := false       -- text read out during an earlier try
  failed : Bool := false
  succeeded : Bool := false
  duplicated : Bool := false       -- ghost: text read out in a later try after text in an earlier one
  dropped : Bool := false          -- a drop happened
  droppedEvery : Bool := false     -- the failing drop came on the third and last try
  droppedAfterText : Bool := false -- a drop happened after text was read out
  deriving DecidableEq, Repr

def active (s : S) : Bool := !s.failed && !s.succeeded

def Spec (s : S) : Bool :=
  !s.duplicated && (!s.failed || s.droppedEvery || s.droppedAfterText)

def onText (s : S) : S :=
  { s with spoke := true, duplicated := s.duplicated || s.spokePrior }

def fail (s : S) : S :=
  { s with failed := true, dropped := true, droppedEvery := s.droppedEvery || s.attempt == .third,
           droppedAfterText := s.droppedAfterText || s.spoke }

/-- The next try: what was read out so far now belongs to an earlier try. -/
def retry (s : S) (next : Attempt) : S :=
  { s with attempt := next, dropped := true, spokePrior := s.spokePrior || s.spoke }

def Attempt.next : Attempt → Option Attempt
  | .first => some .second
  | .second => some .third
  | .third => none

/-! ## The code before the fix: no retry -/

def Cur.step (s : S) : Event → S
  | .drop => if active s then fail s else s
  | .text => if active s then onText s else s
  | .done => if active s then { s with succeeded := true } else s

def Cur.run (evs : List Event) : S := evs.foldl Cur.step {}

/-- One drop, before any text, and the turn is dead. -/
theorem current_violates :
    let s := Cur.run [.drop]
    s.failed = true ∧ s.droppedEvery = false ∧ s.droppedAfterText = false ∧ Spec s = false := by
  decide

/-! ## The code between the two fixes: one retry, at once, on the same client -/

def Once.step (s : S) : Event → S
  | .drop => if active s then
      (if s.attempt == .first && !s.spoke then retry s .second else fail s) else s
  | .text => if active s then onText s else s
  | .done => if active s then { s with succeeded := true } else s

def Once.run (evs : List Event) : S := evs.foldl Once.step {}

/-- 2026-09-30 11:11:31: the retry, sent 120 ms later on the same pool, dropped too, and the turn died. -/
theorem once_violates :
    let s := Once.run [.drop, .drop]
    s.failed = true ∧ s.droppedEvery = false ∧ s.droppedAfterText = false ∧ Spec s = false := by
  decide

/-! ## The obvious alternative: always retry -/

def Naive.step (s : S) : Event → S
  | .drop => if active s then
      (match s.attempt.next with
       | some n => retry s n
       | none => fail s) else s
  | .text => if active s then onText s else s
  | .done => if active s then { s with succeeded := true } else s

def Naive.run (evs : List Event) : S := evs.foldl Naive.step {}

/-- A sentence is read out, the connection drops, the retry reads the same sentence out again. -/
theorem naive_violates :
    let s := Naive.run [.text, .drop, .text]
    s.duplicated = true ∧ Spec s = false := by
  decide

/-! ## The fix: retry twice (after a pause, on a new client), only while nothing has been read out -/

def Fix.step (s : S) : Event → S
  -- computer_agent.py: `if attempt == TLS_RETRIES or spoke: raise`, else replace the client, sleep, retry
  | .drop => if active s then
      (match s.attempt.next with
       | some n => if !s.spoke then retry s n else fail s
       | none => fail s)
      else s
  | .text => if active s then onText s else s
  | .done => if active s then { s with succeeded := true } else s

def Fix.run (evs : List Event) : S := evs.foldl Fix.step {}

def Inv (s : S) : Bool :=
  Spec s &&
  (s.attempt == .first || s.dropped) &&
  !s.spokePrior &&
  (!s.droppedEvery || s.dropped) &&
  (!s.droppedAfterText || (s.spoke && s.dropped))

theorem inv_step : ∀ (s : S) (e : Event), Inv s = true → Inv (Fix.step s e) = true := by
  intro s e
  rcases s with ⟨at_, a, b, c, d, f, g, h, i⟩
  cases at_ <;> cases a <;> cases b <;> cases c <;> cases d <;> cases f <;> cases g <;> cases h <;>
    cases i <;> cases e <;> decide

theorem inv_run (evs : List Event) : ∀ s : S, Inv s = true → Inv (evs.foldl Fix.step s) = true := by
  induction evs with
  | nil => intro s h; exact h
  | cons e rest ih => intro s h; exact ih _ (inv_step s e h)

/-- For every sequence of drops, deltas and completions: text is never read out twice, and a turn fails
only after a drop on each of the three tries or a drop that came after text was read out. -/
theorem fixed_invariant (evs : List Event) : Spec (Fix.run evs) = true := by
  have h := inv_run evs {} (by decide)
  have spec_of_inv : ∀ s : S, Inv s = true → Spec s = true := by
    intro s; unfold Inv; cases Spec s <;> simp
  exact spec_of_inv _ h

/-- The drop that killed the turn, on the fixed code: the retry completes it. -/
theorem fixed_replays_counterexample :
    let s := Fix.run [.drop, .text, .done]
    s.succeeded = true ∧ s.failed = false ∧ s.duplicated = false := by
  decide

/-- The 2026-09-30 trace, a drop and then a drop on the retry, on the fixed code: the third try completes. -/
theorem fixed_survives_two_drops :
    let s := Fix.run [.drop, .drop, .text, .done]
    s.succeeded = true ∧ s.failed = false ∧ s.duplicated = false := by
  decide

/-- Three drops in a row is an outage: the turn fails, and the spec allows it. -/
theorem fixed_three_drops_fail :
    let s := Fix.run [.drop, .drop, .drop]
    s.failed = true ∧ s.droppedEvery = true ∧ Spec s = true := by
  decide

/-- The naive counterexample, on the fixed code: the turn fails rather than repeat the sentence. -/
theorem fixed_never_repeats :
    let s := Fix.run [.text, .drop, .text]
    s.failed = true ∧ s.duplicated = false := by
  decide

end Eve.ResponseRetry
