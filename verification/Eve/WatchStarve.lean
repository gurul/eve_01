/-
  One broken watch and the rest of the list (bridge/src/eve_bridge/watch.py, `Watcher.tick`, `_run_one`,
  `check`, `run`). Line numbers are those of watch.py as reviewed (1792 lines, before the fix).

  `tick` (:1491-1507) walks the watches sorted by `next_at` and checks each one that is due. `check` (:1427-1461)
  moves `next_at` on a reading (:1460) and on a FetchError (:1450), but a read that raises anything else leaves
  `check` with `next_at` unchanged; `_run_one` (:1474-1489) catches only FetchError, so the exception leaves
  `tick` too, and `run` (:1513-1520) logs it and ticks again 60 s later. Such reads exist: `http_request`
  (:644-674) lets out `LookupError` for a `charset=` Python does not know (:674) and `http.client.BadStatusLine`
  or `LineTooLong` for a malformed answer (neither is an OSError), and they come from whoever serves the page.
  The watch keeps the oldest `next_at`, so it is first again on every tick. The property:

    every watch that is due when a tick starts is taken up by that tick: checked, counted as an error, or
    pushed back by the limiter. (So a due watch never waits behind another one for longer than a tick.)

  Abstraction. A watch is `due` or not, with its successful `checks` and its `errors`. One tick is one pass over
  the list in its sorted order; the list's order in the model is that order. `tick os` gives each watch in turn
  its outcome: `ok` (a reading, :1452-1461), `fetchErr` (FetchError, :1435-1451), `limited` (the limiter says
  wait, :1498-1501, next_at pushed back) or `crash` (any other exception). A missing outcome is `ok`. `pass m`
  is time passing: the watches marked in `m` become due. Two ghosts per watch: `wasDue` (due when this tick
  reached the list) and `seen` (this tick took it up). The first check of `add` (:1630-1680) is left out: it
  runs one watch, not the list.

  The fix (a `except Exception` in `check` that counts the error, moves `next_at` as a FetchError does, and
  re-raises it as a FetchError) makes a crash one more `fetchErr`. Bound: none. `fixed_invariant` holds for
  every list of watches, in any order, and every trace of any length, by induction on the list and the trace.

  Where the fix is (re-verification, 2026-09-25): not in `check`, as the paragraph above proposed, but in
  `_run_one` (`except Exception`: the error is counted, `next_at` moves by the same stretch as a FetchError, the
  tick goes on) and in `add` (a first check that raises keeps the watch, "the first check failed"). The rule now
  runs before the error streak is cleared, so a rule that raises adds to the streak. The stretch's exponent is
  capped, since 2.0 ** 1024 overflowed at the 1025th failure and raised out of the except itself.

  Paused watches (2026-09-25, later): a watch the owner paused is not due, whatever its next_at, until it is
  resumed or its pause runs out; tick skips it and `_run_one` refuses it. So "due" in the property reads "due
  and not paused". A watch whose window ended is removed at the top of the tick, before any check.
-/
namespace Eve.WatchStarve

inductive Outcome where
  | ok
  | fetchErr
  | limited
  | crash
  deriving DecidableEq, Repr

structure W where
  due : Bool
  checks : Nat := 0
  errors : Nat := 0
  wasDue : Bool := false      -- ghost: due when this tick reached the list
  seen : Bool := false        -- ghost: this tick took it up
  deriving DecidableEq, Repr

inductive Event where
  | tick (os : List Outcome)
  | pass (m : List Bool)
  deriving DecidableEq, Repr

/-- The property, after a tick: every watch due at its start was taken up. -/
def Spec : List W → Bool
  | [] => true
  | w :: ws => (!w.wasDue || w.seen) && Spec ws

/-- A watch the tick never reached: its ghosts say so. -/
def untouched (w : W) : W := { w with wasDue := w.due, seen := false }

/-- One watch taken up with outcome `o`, the same in both versions except for `crash`. -/
def take (w : W) : Outcome → W
  | .ok => { w with due := false, checks := w.checks + 1, wasDue := true, seen := true }      -- :1457-1460
  | .fetchErr => { w with due := false, errors := w.errors + 1, wasDue := true, seen := true } -- :1448-1450
  | .limited => { w with due := false, wasDue := true, seen := true }                         -- :1499-1501
  | .crash => { w with wasDue := true, seen := true }                                         -- next_at kept

def headOr : List Outcome → Outcome × List Outcome
  | [] => (.ok, [])
  | o :: os => (o, os)

/-! ## The current code -/

/-- `tick`'s loop: a crash leaves the loop at once; the watches after it are not reached. -/
def Cur.go : List W → List Outcome → List W
  | [], _ => []
  | w :: ws, os =>
    let (o, rest) := headOr os
    if w.due then
      match o with
      | .crash => take w .crash :: ws.map untouched
      | o => take w o :: Cur.go ws rest
    else untouched w :: Cur.go ws rest

def passTime : List W → List Bool → List W
  | [], _ => []
  | w :: ws, [] => w :: ws
  | w :: ws, m :: ms => { w with due := w.due || m } :: passTime ws ms

def Cur.step (ws : List W) : Event → List W
  | .tick os => Cur.go ws os
  | .pass m => passTime ws m

def Cur.run (ws : List W) (evs : List Event) : List W := evs.foldl Cur.step ws

def a0 : W := { due := true }       -- its page answers `charset=bogus-8`
def b0 : W := { due := true }       -- a healthy watch, next in the sorted order

/-- The broken watch first, a healthy one behind it: the healthy one was due and the tick never reached it. -/
theorem current_violates : Spec (Cur.step [a0, b0] (.tick [.crash, .ok])) = false := by
  decide

/-- Three ticks, 60 s apart as `run` spaces them: the healthy watch is never checked and the broken one is
never counted, so the owner is never told either (`TELL_AFTER_ERRORS`, :1482). -/
theorem current_starves :
    let s := Cur.run [a0, b0] [.tick [.crash, .ok], .pass [false, false], .tick [.crash, .ok],
                               .pass [false, false], .tick [.crash, .ok]]
    (s.map (·.checks)) = [0, 0] ∧ (s.map (·.errors)) = [0, 0] ∧ (s.map (·.due)) = [true, true] := by
  decide

/-! ## The fixed code -/

/-- A crash is counted and rescheduled like a FetchError, and the loop goes on. -/
def Fix.go : List W → List Outcome → List W
  | [], _ => []
  | w :: ws, os =>
    let (o, rest) := headOr os
    if w.due then
      match o with
      | .crash => take w .fetchErr :: Fix.go ws rest
      | o => take w o :: Fix.go ws rest
    else untouched w :: Fix.go ws rest

def Fix.step (ws : List W) : Event → List W
  | .tick os => Fix.go ws os
  | .pass m => passTime ws m

def Fix.run (ws : List W) (evs : List Event) : List W := evs.foldl Fix.step ws

theorem take_ok (w : W) (o : Outcome) (h : o ≠ .crash) : (!(take w o).wasDue || (take w o).seen) = true := by
  cases o <;> simp_all [take]

theorem fix_go_spec : ∀ (ws : List W) (os : List Outcome), Spec (Fix.go ws os) = true := by
  intro ws
  induction ws with
  | nil => intro os; rfl
  | cons w ws ih =>
    intro os
    cases os with
    | nil =>
      cases hd : w.due <;> simp [Fix.go, headOr, hd, Spec, untouched, take, ih]
    | cons o rest =>
      cases hd : w.due <;> cases o <;> simp [Fix.go, headOr, hd, Spec, untouched, take, ih]

theorem pass_spec : ∀ (ws : List W) (m : List Bool), Spec (passTime ws m) = Spec ws := by
  intro ws
  induction ws with
  | nil => intro m; cases m <;> rfl
  | cons w ws ih =>
    intro m
    cases m with
    | nil => rfl
    | cons b ms => simp [passTime, Spec, ih]

/-- For every list of watches in any order and every trace of ticks and time passing: after each tick, every
watch that was due when it started was taken up. -/
theorem fixed_invariant (ws : List W) (evs : List Event) (h : Spec ws = true) :
    Spec (Fix.run ws evs) = true := by
  induction evs generalizing ws with
  | nil => exact h
  | cons e rest ih =>
    apply ih
    cases e with
    | tick os => exact fix_go_spec ws os
    | pass m => rw [Fix.step, pass_spec]; exact h

/-- The trace that starves the current code, on the fixed one: both watches are taken up every tick. -/
theorem fixed_replays_counterexample :
    let s := Fix.run [a0, b0] [.tick [.crash, .ok], .pass [true, false], .tick [.crash, .ok]]
    (s.map (·.checks)) = [0, 1] ∧ (s.map (·.errors)) = [2, 0] := by
  decide

end Eve.WatchStarve
