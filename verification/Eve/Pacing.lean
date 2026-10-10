/-
  How far eve's speech runs ahead of the Voice PE's speaker (bridge/src/eve_bridge/desk_call.py,
  class BoardSpeaker: `ahead`, `play`, `flush`).

  `play` sends the reply to the board in pieces of at most PCM_PIECE_BYTES (100 ms of audio). Speech
  arrives faster than it plays and the board's buffer holds 12 s, so the daemon paces itself. The property:

    after every event, the daemon is at most LEAD_SECS (3 s) ahead of the board's playback,
    where `ahead()` is `max(0, queued - (clock - started))`.

  The bug. The first `play` slept for `wait = ahead - LEAD` when that was positive, then sent the piece.
  With `ahead` exactly LEAD, `wait` is 0, the piece goes, and `ahead` becomes LEAD plus one piece. The
  counterexample (`current_violates`): 31 pieces of 100 ms from rest, with no time passing, leave the daemon
  3.1 s ahead. The pytest `test_the_daemon_stays_at_most_three_seconds_ahead_of_the_board`
  (bridge/tests/test_desk_call.py) replays it: 10 s of audio, 100 pieces, on a fake clock that moves only
  when the daemon sleeps. On the old code it ended 3.1 s ahead (3.1 > 3.0) after 6.9 s of sleep; the fix
  ends 3.0 s ahead after 7.0 s of sleep (`replays_the_pytest`).

  The fix (desk_call.py `play`, the line `wait = self.ahead() + len(piece) / BYTES_PER_SEC - LEAD_SECS`):
  make room for the whole piece before it goes, `wait = ahead + piece - LEAD`. `fixed_invariant` proves
  `ahead <= LEAD` after every event of every trace, for every LEAD and every piece p with p <= LEAD.

  Abstraction.
  * Time is a natural number of 100 ms ticks. LEAD_SECS is 30 ticks; the theorems take any LEAD. A piece
    is p ticks; the Python's pieces are 1 tick, and the last one of a reply can be shorter (it is still at
    most 1 tick). The Python uses floats; the pytest allows 1e-9 of rounding, which ticks do not have.
  * Nat subtraction is the `max(0, ...)` of `ahead` and of `if wait > 0`: a truncated `wait` of 0 is no sleep.
  * `started = None` is modelled as `started := now, queued := 0`. Both give `ahead = 0`, and the next
    piece starts a new burst either way. `flush` sets it so.
  * An event is the clock moving on by d (`tick d`), `flush`, or one piece of `play` (`send p late`):
      - if `ahead` is 0 the board ran dry, and a new burst starts: `started = now, queued = 0`;
      - the daemon sleeps `wait` (`asyncio.sleep`);
      - `late` is any further time that passes before `queued` grows: an oversleep, or the `await send`.
        The Python's `asyncio.sleep` never returns early, so `late >= 0` covers every real schedule;
      - `queued += p`.
    The loop in `play` is a run of `send` events; time between replies is `tick`.
  * The two `self.clock()` calls in one piece (the reset check and the `wait`) have no `await` between
    them, so they read the same instant; the model reads `now` once.
  * The property is checked after each event. Inside a sleep, `ahead` only falls.
-/
namespace Eve.Pacing

/-- LEAD_SECS in 100 ms ticks, desk_call.py:126. -/
def LEAD : Nat := 30

structure S where
  now : Nat          -- `self.clock()`
  started : Nat      -- `self.started` (None: see the docstring)
  queued : Nat       -- `self.queued`, in ticks
  deriving DecidableEq, Repr

inductive Event where
  | tick (d : Nat)             -- time passes: the board plays
  | send (p late : Nat)        -- one piece of `play`, desk_call.py:148-155
  | flush                      -- `flush`, desk_call.py:157-159
  deriving DecidableEq, Repr

/-- `ahead`, desk_call.py:140-144: `max(0, queued - (clock - started))`. -/
def ahead (s : S) : Nat := s.queued - (s.now - s.started)

/-- The sleep before a piece of p ticks. The old code: `ahead - LEAD`. The fix (desk_call.py:151):
`ahead + piece - LEAD`. Nat subtraction gives 0 where the Python's `if wait > 0` does not sleep. -/
def wait (fix : Bool) (L : Nat) (s : S) (p : Nat) : Nat :=
  if fix then ahead s + p - L else ahead s - L

/-- The board ran dry: a new burst starts now, desk_call.py:149-150. -/
def burst (s : S) : S :=
  if ahead s = 0 then { s with started := s.now, queued := 0 } else s

def step (fix : Bool) (L : Nat) (s : S) : Event → S
  | .tick d => { s with now := s.now + d }
  | .flush => { s with started := s.now, queued := 0 }
  | .send p late =>
      let s1 := burst s
      -- sleep `wait` (desk_call.py:152-153), send (154), then `queued += piece` (155)
      { s1 with now := s1.now + wait fix L s1 p + late, queued := s1.queued + p }

/-- At rest: nothing sent, the clock at t. -/
def rest (t : Nat) : S := { now := t, started := t, queued := 0 }

def Spec (L : Nat) (s : S) : Bool := decide (ahead s ≤ L)

/-! ## The current code -/

def Cur.run (L t : Nat) (evs : List Event) : S := evs.foldl (step false L) (rest t)

/-- 31 pieces of 100 ms from rest, no time passing: the old step never sleeps (the 31st piece goes when
`ahead` is exactly 3.0 s), and the daemon ends 3.1 s ahead of the board. -/
theorem current_violates :
    let s := Cur.run LEAD 0 (List.replicate 31 (.send 1 0))
    ahead s = 31 ∧ s.now = 0 ∧ Spec LEAD s = false := by
  decide +kernel

/-! ## The fixed code -/

def Fix.run (L t : Nat) (evs : List Event) : S := evs.foldl (step true L) (rest t)

def Inv (L : Nat) (s : S) : Prop := s.started ≤ s.now ∧ ahead s ≤ L

theorem inv_step (L : Nat) (s : S) (e : Event) (h : Inv L s)
    (hp : ∀ p late, e = .send p late → p ≤ L) : Inv L (step true L s e) := by
  obtain ⟨h1, h2⟩ := h
  cases e with
  | tick d =>
    simp only [Inv, step, ahead] at *
    omega
  | flush =>
    simp only [Inv, step, ahead]
    omega
  | send p late =>
    have hpL := hp p late rfl
    by_cases h0 : ahead s = 0
    · have hb : burst s = { s with started := s.now, queued := 0 } := by simp [burst, h0]
      simp only [Inv, step, hb, wait, ahead, ite_true] at *
      omega
    · have hb : burst s = s := by simp [burst, h0]
      simp only [Inv, step, hb, wait, ahead, ite_true] at *
      omega

theorem inv_run (L : Nat) (evs : List Event) (hp : ∀ p late, Event.send p late ∈ evs → p ≤ L) :
    ∀ s, Inv L s → Inv L (evs.foldl (step true L) s) := by
  induction evs with
  | nil => intro s h; exact h
  | cons e rest ih =>
    intro s h
    have hs := inv_step L s e h (fun p late he => hp p late (he ▸ List.mem_cons_self))
    exact ih (fun p late hm => hp p late (List.mem_cons_of_mem _ hm)) _ hs

/-- For every LEAD, every start time and every trace of time passing, flushes and pieces, each piece at
most LEAD: after the whole trace the fixed daemon is at most LEAD ahead of the board. Every prefix of a
trace is a trace, so this holds after every event (`fixed_every_prefix`). -/
theorem fixed_invariant (L t : Nat) (evs : List Event) (hp : ∀ p late, Event.send p late ∈ evs → p ≤ L) :
    Spec L (Fix.run L t evs) = true := by
  have h := inv_run L evs hp (rest t) ⟨Nat.le_refl t, by simp [ahead, rest]⟩
  show decide (ahead ((evs.foldl (step true L) (rest t))) ≤ L) = true
  exact decide_eq_true h.2

theorem fixed_every_prefix (L t : Nat) (evs : List Event) (hp : ∀ p late, Event.send p late ∈ evs → p ≤ L)
    (k : Nat) : Spec L (Fix.run L t (evs.take k)) = true :=
  fixed_invariant L t (evs.take k) (fun p late hm => hp p late (List.mem_of_mem_take hm))

/-- The fixed step never sleeps when the board is empty: a new burst's first piece goes at once. -/
theorem fixed_no_sleep_when_empty (L : Nat) (s : S) (p : Nat) (hp : p ≤ L) (h : ahead s = 0) :
    wait true L (burst s) p = 0 := by
  have hb : burst s = { s with started := s.now, queued := 0 } := by simp [burst, h]
  simp only [hb, wait, ahead, ite_true]
  omega

/-- The pytest, replayed: 10 s of audio (100 pieces) on a clock at 100 s (1000 ticks) that moves only when
the daemon sleeps. The old step ends 3.1 s ahead after 6.9 s of sleep; the fixed one 3.0 s after 7.0 s,
inside the test's 6.8..7.1. -/
theorem replays_the_pytest :
    let c := Cur.run LEAD 1000 (List.replicate 100 (.send 1 0))
    let f := Fix.run LEAD 1000 (List.replicate 100 (.send 1 0))
    ahead c = 31 ∧ c.now - 1000 = 69 ∧ ahead f = 30 ∧ f.now - 1000 = 70 := by
  decide +kernel

end Eve.Pacing
