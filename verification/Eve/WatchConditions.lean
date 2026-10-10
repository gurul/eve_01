/-
  The watcher's conditions: `evaluate(w, r)` in bridge/src/eve_bridge/watch.py:984-1060, and the
  state it keeps on the watch (`checks`, `armed`, `baseline`, `last_value`, `last_available`).

  The properties, over any sequence of readings (and restarts, see below):

   1. the first reading (`checks == 0`) never alerts, for any condition;
   2. below / above / available / appears are edge-triggered: an alert comes only on a reading where
      the condition holds, and between two alerts there is a reading that carried the field and on which
      the condition did NOT hold (it re-armed); a reading whose field is None neither alerts nor touches
      `armed`; and a re-armed condition that holds again always alerts;
   3. drop_pct / rise_pct: an alert comes exactly when the move from the baseline reaches the
      percentage, the baseline then becomes that reading, and otherwise it stays; once a reading has
      carried a price the baseline is never None; and a reading never raises;
   4. change: an alert exactly when the value differs from the last KNOWN value, or the availability
      differs from the last known availability, never on the first reading.

  Finding (a bug, shape (a)). In 3, the current code divides by the baseline BEFORE its `base > 0`
  guard (watch.py:1023-1024). A price of 0 is a legal reading (`_price` and `model_reading` keep any
  finite value >= 0), so a baseline of 0 is reachable two ways: a first priced reading of 0 (rise_pct
  or drop_pct), or a drop_pct alert on a drop to 0 (a 100% drop reaches any percentage < 100, which
  `validate` requires), which sets the baseline to 0. The next priced reading raises ZeroDivisionError
  out of `evaluate`, out of `Watcher.check` (watch.py:1457, only FetchError is caught) and out of
  `tick` (watch.py:1503): the watch's `next_at` is never advanced (watch.py:1460 is not reached) and
  the list is not saved, so it stays first in line and every later tick dies on it again: no other
  watch is checked. The baseline 0 was saved by the check that set it, so a restart does not clear it.
  The fix treats a baseline of 0 like no baseline: the reading becomes the baseline (watch.py:1019).

  Not a bug, and why the Spec says so (2). The literal "alerts <= false->true transitions of the
  condition over the readings that carry the field" FAILS on the current code: a first reading whose
  field is None leaves `armed` at its default True (the branch guard at 1007/1035/1044 skips it), so
  the first reading that carries the field and already holds alerts, with no false reading before it
  (`unknown_start_alerts`). This is the intended reading of the code, not a defect: `add` reports the
  first reading to the owner ("already holds", watch.py `met`), and when that first reading carried
  nothing the owner was told "no reading yet", so the first time the condition is known to hold is
  news (tickets found on sale after a search that could not tell). The exception is exact and
  one-shot: it happens only when no earlier reading carried the field, so at most once per watch
  (`freeUsed` below), and the property proved is "a known false reading, or no known reading at
  all, since the last alert". A reviewer who wants the literal form instead would arm from the first
  reading that carries the field; that is a product decision, recorded here, not a proof gap.

  Also checked (3): a first reading whose value is None, then a priced one: `w.baseline is None`
  (1019) makes that priced reading the baseline, with no alert. Such first readings do happen: a page
  whose structured data, text model and browser all find no price returns the reading anyway
  (`_read_page`), and a search reading's price can be null (`model_reading`).

  Restarts. `to_json` is `dataclasses.asdict` (every field), and `from_json` passes every field name
  back to the constructor (watch.py:921-938); JSON keeps floats, bools and None exactly. So a restart
  maps the state `evaluate` depends on (`checks`, `armed`, `baseline`, `last_value`,
  `last_available`) to itself; `restart` below is that round trip, written out field by field. The
  test file checks the round trip on the real class.

  Abstraction and bounds.
   * Edge conditions: a reading is abstracted to what the branch reads: the field is None, or present
     and the condition holds / fails (`r.value <= w.value` for below, `>=` for above, `r.available`,
     `r.hit`). `w.value` is never None for below/above (`validate` requires a positive mark). The
     state is finite, so the invariant is checked on every state and event by `decide`: no bound on
     the trace length.
   * drop/rise: prices are naturals (any size) and the percentage is a positive natural. The move is
     compared exactly (`reaches`, cross-multiplied in Int); for a positive base that is the Python
     `moved >= w.value` over the reals. Float rounding is not modelled (see the answer: an exact 10%
     drop from 3.00 to 2.70 computes 9.999999999999996 and does not fire). No bound on trace length.
   * change: values are naturals, so "differs by more than 1e-9" is `≠`. No bound on trace length.
   * The alert text, the currency, the link and `fired` are not modelled: only whether an alert is
     returned.

  Since this model (re-verification, 2026-09-25), two things the Python does that the model does not say:
  * Property 1 is about a first reading the owner was SHOWN (`evaluate(..., shown=True)`, as `add` reports it).
    A first reading the owner never saw (the add's own check failed or was queued; the loop calls `shown=False`)
    may fire an edge condition that already holds. That is WatchTicketmaster's fixed step, proved there.
  * The percentage test is `moved >= value - 1e-9` in the Python and exact here: a move within 1e-9 percentage
    points of the mark counts as reaching it (3.00 -> 2.70 is 9.999...% in floats). Deliberate, and the only
    extra alerts it adds are on moves that short of the mark.
  * A reading in another currency is a failed read in `check` before `evaluate` runs; `evaluate` itself sees it
    as a reading with no price, which is the modelled `none`.
-/
namespace Eve.WatchConditions

/-! ## 2. Edge-triggered conditions: below, above, available, appears -/

/-- What one reading says about the watched field. -/
inductive Obs where
  | none    -- the field is None: `r.value` / `r.available` / `r.hit` read nothing
  | holds   -- the field is there and the condition holds
  | fails   -- the field is there and the condition does not hold
  deriving DecidableEq, Repr

inductive Event where
  | read (o : Obs)
  | restart
  deriving DecidableEq, Repr

/-- The state `evaluate` keeps for an edge condition. -/
structure Edge where
  started : Bool := false   -- `w.checks > 0`
  armed : Bool := true      -- `w.armed`, default True (watch.py:908)
  deriving DecidableEq, Repr

/-- What `save` writes and `_load` reads back for it. -/
structure EdgeJson where
  checks_pos : Bool
  armed : Bool
  deriving DecidableEq, Repr

def Edge.toJson (c : Edge) : EdgeJson := ⟨c.started, c.armed⟩
def Edge.ofJson (j : EdgeJson) : Edge := ⟨j.checks_pos, j.armed⟩

/-- `evaluate` for one of the four edge conditions: the new state and whether it alerted. -/
def Edge.read (c : Edge) (o : Obs) : Edge × Bool :=
  let first := !c.started                           -- 988
  let c1 := { c with started := true }              -- 990
  match o with
  | .none => (c1, false)                            -- 1007/1035/1044: the branch is skipped
  | .holds =>
      if first then ({ c1 with armed := false }, false)          -- 1010/1037/1046: armed = not holds
      else if c.armed then ({ c1 with armed := false }, true)    -- 1011-1015 / 1038-1041 / 1047-1049
      else (c1, false)
  | .fails =>
      if first then ({ c1 with armed := true }, false)           -- armed = not holds
      else ({ c1 with armed := true }, false)                    -- 1016-1017 / 1042-1043 / 1050-1051

/-- The observer: facts about the readings so far, kept apart from the code's own state. -/
structure EdgeMon where
  known : Bool := false      -- some reading carried the field
  sawFalse : Bool := false   -- since the last alert (or the start), a reading carried the field and failed
  freeUsed : Bool := false   -- an alert came with no known false reading before it (the unknown start)
  bad : Bool := false        -- some reading broke the property
  deriving DecidableEq, Repr

def EdgeMon.observe (m : EdgeMon) (pre post : Edge) (o : Obs) (alert : Bool) : EdgeMon :=
  -- alert exactly when: not the first reading, the condition holds, and it re-armed since the last
  -- alert (or nothing was ever known)
  let want := pre.started && o == .holds && (m.sawFalse || !m.known)
  let ok :=
    alert == want &&
    -- a None field neither alerts nor touches the arming
    (o != .none || (!alert && post.armed == pre.armed)) &&
    -- the unknown-start exception happens at most once per watch
    (!(alert && !m.sawFalse) || !m.freeUsed)
  { known := m.known || o != .none,
    sawFalse := if alert then false else (m.sawFalse || o == .fails),
    freeUsed := m.freeUsed || (alert && !m.sawFalse),
    bad := m.bad || !ok }

abbrev EdgeS := Edge × EdgeMon

def EdgeS.step (s : EdgeS) : Event → EdgeS
  | .read o => let r := s.1.read o
               (r.1, s.2.observe s.1 r.1 o r.2)
  | .restart => (Edge.ofJson s.1.toJson, s.2)

def EdgeS.run (evs : List Event) : EdgeS := evs.foldl EdgeS.step ({}, {})

def EdgeSpec (s : EdgeS) : Bool := !s.2.bad

/-- What every reachable state satisfies. -/
def EdgeInv (s : EdgeS) : Bool :=
  EdgeSpec s &&
  (s.1.armed == (s.2.sawFalse || !s.2.known)) &&
  (s.1.started || !s.2.known) &&
  (!s.2.freeUsed || s.2.known) &&
  (!s.2.sawFalse || s.2.known)

theorem edge_inv_step : ∀ (s : EdgeS) (e : Event), EdgeInv s = true → EdgeInv (s.step e) = true := by
  intro s e
  rcases s with ⟨⟨a, b⟩, ⟨c, d, f, g⟩⟩
  cases a <;> cases b <;> cases c <;> cases d <;> cases f <;> cases g <;>
    (first | cases e with | read o => cases o <;> decide | restart => decide)

theorem edge_inv_run (evs : List Event) :
    ∀ s : EdgeS, EdgeInv s = true → EdgeInv (evs.foldl EdgeS.step s) = true := by
  induction evs with
  | nil => intro s h; exact h
  | cons e rest ih => intro s h; exact ih _ (edge_inv_step s e h)

/-- Property 1 and 2 for below/above/available/appears, over every trace of readings and restarts:
no alert on the first reading, an alert exactly when a re-armed condition holds, a None field is inert,
and the one alert not preceded by a known false reading can happen only once. -/
theorem edge_invariant (evs : List Event) : EdgeSpec (EdgeS.run evs) = true := by
  have h := edge_inv_run evs ({}, {}) (by decide)
  unfold EdgeInv at h
  cases hs : EdgeSpec (EdgeS.run evs) <;> simp_all [EdgeS.run]

/-- The literal "alerts <= false->true transitions over readings that carry the field" is not what the
code does: an unknown first reading, then one that holds, alerts (the intended "news" edge). -/
theorem unknown_start_alerts :
    let s := EdgeS.run [.read .none]
    (s.1.read .holds).2 = true ∧ s.2.known = false := by
  decide

/-- A condition that already holds at the first reading waits, across a restart, for it to stop
holding and hold again. -/
theorem held_at_start_waits_across_restart :
    let s := EdgeS.run [.read .holds, .restart]
    (s.1.read .holds).2 = false ∧
      ((EdgeS.run [.read .holds, .restart, .read .fails]).1.read .holds).2 = true := by
  decide

/-! ## 3. drop_pct and rise_pct -/

inductive Dir where
  | drop
  | rise
  deriving DecidableEq, Repr

/-- The move from `base` to `v` reaches `pct` percent, exactly: for base > 0 this is
`(base - v) / base * 100 >= pct` (drop) or `(v - base) / base * 100 >= pct` (rise) over the reals. -/
def reaches (d : Dir) (pct base v : Nat) : Bool :=
  match d with
  | .drop => decide ((pct : Int) * base ≤ ((base : Int) - v) * 100)
  | .rise => decide ((pct : Int) * base ≤ ((v : Int) - base) * 100)

structure Pct where
  started : Bool := false          -- `w.checks > 0`
  baseline : Option Nat := none    -- `w.baseline`
  deriving DecidableEq, Repr

structure PctJson where
  checks_pos : Bool
  baseline : Option Nat
  deriving DecidableEq, Repr

def Pct.toJson (c : Pct) : PctJson := ⟨c.started, c.baseline⟩
def Pct.ofJson (j : PctJson) : Pct := ⟨j.checks_pos, j.baseline⟩

/-- One reading's outcome: the state after, whether it alerted, whether `evaluate` raised. -/
structure Out where
  post : Pct
  alert : Bool
  raised : Bool

/-- The current code, watch.py:1018-1027. `p` is `w.value`; the branch needs it truthy. -/
def Cur.pct (d : Dir) (p : Nat) (c : Pct) : Option Nat → Out
  | none => ⟨{ c with started := true }, false, false⟩          -- 1018: `r.value is not None` fails
  | some x =>
      let c1 := { c with started := true }
      if p = 0 then ⟨c1, false, false⟩ else                    -- 1018: `and w.value`
      match c.baseline with
      | none => ⟨{ c1 with baseline := some x }, false, false⟩  -- 1019-1020: `w.baseline is None`
      | some b =>
          if !c.started then ⟨{ c1 with baseline := some x }, false, false⟩   -- 1019: `or first`
          -- 1023: `moved = (...) / base * 100` divides first: base == 0 raises ZeroDivisionError,
          -- before the `base > 0` guard on 1024 is reached. checks and last_value are already written.
          else if b = 0 then ⟨c1, false, true⟩
          else if reaches d p b x then ⟨{ c1 with baseline := some x }, true, false⟩   -- 1024-1027
          else ⟨c1, false, false⟩

/-- The fixed code: `if w.baseline is None or first or w.baseline <= 0: w.baseline = r.value`, and the
now redundant `base > 0 and` dropped from the test. -/
def Fix.pct (d : Dir) (p : Nat) (c : Pct) : Option Nat → Out
  | none => ⟨{ c with started := true }, false, false⟩
  | some x =>
      let c1 := { c with started := true }
      if p = 0 then ⟨c1, false, false⟩ else
      match c.baseline with
      | none => ⟨{ c1 with baseline := some x }, false, false⟩
      | some b =>
          if !c.started || b = 0 then ⟨{ c1 with baseline := some x }, false, false⟩
          else if reaches d p b x then ⟨{ c1 with baseline := some x }, true, false⟩
          else ⟨c1, false, false⟩

structure PctMon where
  priced : Bool := false    -- some reading carried a price
  bad : Bool := false
  deriving DecidableEq, Repr

def PctMon.observe (d : Dir) (p : Nat) (m : PctMon) (pre : Pct) (v : Option Nat) (o : Out) : PctMon :=
  -- alert exactly when a positive baseline exists (not the first reading) and the move reaches p
  let want := match v, pre.baseline with
    | some x, some b => pre.started && b != 0 && reaches d p b x
    | _, _ => false
  let priced := m.priced || v.isSome
  let ok :=
    !o.raised &&
    o.alert == want &&
    (!o.alert || o.post.baseline == v) &&                     -- the baseline becomes that reading
    (v.isSome || o.post.baseline == pre.baseline) &&          -- a None price leaves it alone
    -- otherwise the baseline stays, unless there was no usable one (None, 0, or the first reading)
    (o.alert || o.post.baseline == pre.baseline || pre.baseline == none ||
      pre.baseline == some 0 || !pre.started) &&
    (!priced || o.post.baseline.isSome)                        -- never None after a priced reading
  { priced := priced, bad := m.bad || !ok }

inductive PEvent where
  | read (v : Option Nat)
  | restart
  deriving DecidableEq, Repr

abbrev PctS := Pct × PctMon

def PctS.stepWith (f : Pct → Option Nat → Out) (d : Dir) (p : Nat) (s : PctS) : PEvent → PctS
  | .read v => let o := f s.1 v
               (o.post, s.2.observe d p s.1 v o)
  | .restart => (Pct.ofJson s.1.toJson, s.2)

def Cur.pctRun (d : Dir) (p : Nat) (evs : List PEvent) : PctS :=
  evs.foldl (PctS.stepWith (Cur.pct d p) d p) ({}, {})

def Fix.pctRun (d : Dir) (p : Nat) (evs : List PEvent) : PctS :=
  evs.foldl (PctS.stepWith (Fix.pct d p) d p) ({}, {})

def PctSpec (s : PctS) : Bool := !s.2.bad

/-- A 10% rise watch whose first price is 0 (a "free" listing, a placeholder price): the next priced
reading raises instead of answering. -/
theorem current_violates :
    let s := Cur.pctRun .rise 10 [.read (some 0)]
    (Cur.pct .rise 10 s.1 (some 5)).raised = true ∧
      PctSpec (Cur.pctRun .rise 10 [.read (some 0), .read (some 5)]) = false := by
  decide

/-- The other way in: a 10% drop watch sees 2, then 0 (a 100% drop: it alerts and the baseline becomes
0), then 1: that reading raises, and after a restart it still does (the 0 baseline was saved). -/
theorem current_violates_after_a_drop_to_zero :
    let s := Cur.pctRun .drop 10 [.read (some 2), .read (some 0)]
    s.1.baseline = some 0 ∧ PctSpec s = true ∧
      (Cur.pct .drop 10 s.1 (some 1)).raised = true ∧
      (Cur.pct .drop 10 (Cur.pctRun .drop 10 [.read (some 2), .read (some 0), .restart]).1 (some 1)).raised
        = true := by
  decide

/-- The None-valued first reading is handled by the current code: the first priced reading becomes the
baseline, and a later move from it alerts. -/
theorem none_first_sets_baseline_later :
    let s := Cur.pctRun .drop 10 [.read none, .read (some 100)]
    s.1.baseline = some 100 ∧ PctSpec s = true ∧ (Cur.pct .drop 10 s.1 (some 90)).alert = true := by
  decide

def PctInv (s : PctS) : Prop :=
  s.2.bad = false ∧ (s.2.priced = true → s.1.baseline.isSome = true)

theorem fix_pct_inv_step (d : Dir) (p : Nat) (hp : 0 < p) :
    ∀ (s : PctS) (e : PEvent), PctInv s → PctInv (PctS.stepWith (Fix.pct d p) d p s e) := by
  intro s e h
  rcases s with ⟨⟨st, bl⟩, ⟨pr, bd⟩⟩
  obtain ⟨hb, hpr⟩ := h
  simp only at hb hpr
  subst hb
  have hp0 : (p = 0) = False := by simp; omega
  cases e with
  | restart => exact ⟨rfl, hpr⟩
  | read v =>
      cases v with
      | none =>
          cases bl <;> cases pr <;> cases st <;>
            simp_all [PctInv, PctS.stepWith, Fix.pct, PctMon.observe]
      | some x =>
          cases bl with
          | none =>
              cases st <;> simp [PctInv, PctS.stepWith, Fix.pct, PctMon.observe, hp0]
          | some b =>
              cases st with
              | false => simp [PctInv, PctS.stepWith, Fix.pct, PctMon.observe, hp0]
              | true =>
                  by_cases hb0 : b = 0
                  · subst hb0; simp [PctInv, PctS.stepWith, Fix.pct, PctMon.observe, hp0]
                  · cases hr : reaches d p b x <;>
                      simp [PctInv, PctS.stepWith, Fix.pct, PctMon.observe, hp0, hb0, hr]

theorem fix_pct_inv_run (d : Dir) (p : Nat) (hp : 0 < p) (evs : List PEvent) :
    ∀ s : PctS, PctInv s → PctInv (evs.foldl (PctS.stepWith (Fix.pct d p) d p) s) := by
  induction evs with
  | nil => intro s h; exact h
  | cons e rest ih => intro s h; exact ih _ (fix_pct_inv_step d p hp s e h)

/-- Property 1 and 3 on the fixed code, for either direction, any positive percentage, any prices and
any trace of readings and restarts: never raises, alerts exactly when the move from a positive baseline
reaches the percentage (never on the first reading), the baseline then becomes that reading and
otherwise stays, and it is never None after a priced reading. -/
theorem fixed_pct_invariant (d : Dir) (p : Nat) (hp : 0 < p) (evs : List PEvent) :
    PctSpec (Fix.pctRun d p evs) = true := by
  have h := fix_pct_inv_run d p hp evs ({}, {}) ⟨rfl, by simp⟩
  simp [PctSpec, Fix.pctRun, h.1]

/-- Both traces that break the current code, on the fixed code: nothing raises, the watch keeps
answering, and a later 10% move from the new baseline alerts. -/
theorem fixed_replays_counterexample :
    PctSpec (Fix.pctRun .rise 10 [.read (some 0), .read (some 5)]) = true ∧
    (Fix.pct .rise 10 (Fix.pctRun .rise 10 [.read (some 0), .read (some 5)]).1 (some 6)).alert = true ∧
    PctSpec (Fix.pctRun .drop 10 [.read (some 2), .read (some 0), .restart, .read (some 1)]) = true := by
  decide

/-! ## 4. change -/

/-- A reading as `change` sees it: the value and the availability, either may be None. -/
abbrev Rd := Option Nat × Option Bool

structure Ch where
  started : Bool := false            -- `w.checks > 0`
  lastValue : Option Nat := none     -- `w.last_value`
  lastAvail : Option Bool := none    -- `w.last_available`
  deriving DecidableEq, Repr

structure ChJson where
  checks_pos : Bool
  last_value : Option Nat
  last_available : Option Bool
  deriving DecidableEq, Repr

def Ch.toJson (c : Ch) : ChJson := ⟨c.started, c.lastValue, c.lastAvail⟩
def Ch.ofJson (j : ChJson) : Ch := ⟨j.checks_pos, j.last_value, j.last_available⟩

def differs {α : Type} [DecidableEq α] : Option α → Option α → Bool
  | some x, some y => decide (x ≠ y)
  | _, _ => false

/-- `evaluate` for `change`, watch.py:988-1000 and 1028-1034. -/
def Ch.read (c : Ch) (r : Rd) : Ch × Bool :=
  let first := !c.started                                  -- 988
  let pv := c.lastValue                                    -- 989: captured BEFORE the update
  let pa := c.lastAvail
  let c1 : Ch := { started := true,                        -- 990
                   lastValue := if r.1.isSome then r.1 else c.lastValue,     -- 997-998
                   lastAvail := if r.2.isSome then r.2 else c.lastAvail }    -- 999-1000
  -- 1029-1034: `if value differs: ... elif availability differs: ...`
  (c1, !first && (differs r.1 pv || differs r.2 pa))

/-- The last known value and availability, from the readings themselves (newest first). -/
def lastKnownV : List Rd → Option Nat
  | [] => none
  | r :: h => match r.1 with
    | some v => some v
    | none => lastKnownV h

def lastKnownA : List Rd → Option Bool
  | [] => none
  | r :: h => match r.2 with
    | some a => some a
    | none => lastKnownA h

structure ChMon where
  hist : List Rd := []     -- every reading so far, newest first
  bad : Bool := false

def ChMon.observe (m : ChMon) (r : Rd) (alert : Bool) : ChMon :=
  let want := !m.hist.isEmpty && (differs r.1 (lastKnownV m.hist) || differs r.2 (lastKnownA m.hist))
  { hist := r :: m.hist, bad := m.bad || alert != want }

inductive CEvent where
  | read (r : Rd)
  | restart
  deriving DecidableEq, Repr

abbrev ChS := Ch × ChMon

def ChS.step (s : ChS) : CEvent → ChS
  | .read r => let o := s.1.read r
               (o.1, s.2.observe r o.2)
  | .restart => (Ch.ofJson s.1.toJson, s.2)

def ChS.run (evs : List CEvent) : ChS := evs.foldl ChS.step ({}, {})

def ChSpec (s : ChS) : Bool := !s.2.bad

def ChInv (s : ChS) : Prop :=
  s.2.bad = false ∧ s.1.lastValue = lastKnownV s.2.hist ∧ s.1.lastAvail = lastKnownA s.2.hist ∧
    s.1.started = !s.2.hist.isEmpty

theorem ch_inv_step : ∀ (s : ChS) (e : CEvent), ChInv s → ChInv (s.step e) := by
  intro s e h
  rcases s with ⟨⟨st, lv, la⟩, ⟨hist, bd⟩⟩
  obtain ⟨hb, hv, ha, hs⟩ := h
  simp only at hb hv ha hs
  subst hb hv ha hs
  cases e with
  | restart => exact ⟨rfl, rfl, rfl, rfl⟩
  | read r =>
      rcases r with ⟨v, a⟩
      cases v <;> cases a <;> simp [ChInv, ChS.step, Ch.read, ChMon.observe, lastKnownV, lastKnownA]

theorem ch_inv_run (evs : List CEvent) : ∀ s : ChS, ChInv s → ChInv (evs.foldl ChS.step s) := by
  induction evs with
  | nil => intro s h; exact h
  | cons e rest ih => intro s h; exact ih _ (ch_inv_step s e h)

/-- Property 1 and 4 for `change`, over every trace of readings and restarts: an alert exactly when the
value differs from the last known value or the availability from the last known availability, never on
the first reading. (The code compares against the value from before this reading's update.) -/
theorem change_invariant (evs : List CEvent) : ChSpec (ChS.run evs) = true := by
  have h := ch_inv_run evs ({}, {}) ⟨rfl, rfl, rfl, rfl⟩
  simp [ChSpec, ChS.run, h.1]

/-! ## Everything, on the fixed code -/

/-- The whole property on the fixed code: the edge conditions and `change` are unchanged by the fix
(`edge_invariant`, `change_invariant`); drop/rise hold with the fix (`fixed_pct_invariant`). -/
theorem fixed_invariant :
    (∀ evs : List Event, EdgeSpec (EdgeS.run evs) = true) ∧
    (∀ evs : List CEvent, ChSpec (ChS.run evs) = true) ∧
    (∀ (d : Dir) (p : Nat), 0 < p → ∀ evs : List PEvent, PctSpec (Fix.pctRun d p evs) = true) :=
  ⟨edge_invariant, change_invariant, fixed_pct_invariant⟩

end Eve.WatchConditions
