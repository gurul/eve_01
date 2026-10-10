/-
  The watcher's rate limiter (bridge/src/eve_bridge/watch.py, class RateLimiter, lines 189-272) and
  the gate the Watcher puts in front of every read (Watcher.tick 1491-1508, Watcher.add 1630-1672).
  Line numbers are those of watch.py as read on 2026-09-25 (untracked on branch feat/watch).

  Properties, in plain words:

    1. The bucket never holds more than `burst` tokens, and a clock that moves back and forth mints no
       token: after any trace, the bucket equals a reference bucket driven by the monotone clock (the
       largest reading the limiter has taken so far).                                      [holds]
    2. A `take` made while the last `ready_in` said 0 (and no take came between) never leaves the bucket
       below 0; only a take without asking first can go into debt.                          [holds]
    3. `penalize` returns a value >= min(retry_after, backoff_max) and <= backoff_max; strikes grow by
       one; the ladder step (the part without Retry-After) never shrinks between clears; the first
       penalty after a clear is min(backoff_max, backoff_base).                             [holds]
       And: a penalty never moves the end of a hold still in force earlier; the server's Retry-After
       stands until a success clears it.                                       [BROKEN, fixed below]
    4. `model_ok` is false exactly when `model_used` was called `cap` times with today's day label;
       a day label never seen before starts at 0.                              [BROKEN, fixed below]
    5. (Watcher) no read goes to a host while the host is held by a penalty.  [BROKEN, fixed below]

  Every theorem is general: any parameters, any trace length. Nothing is bounded, except the concrete
  counterexample traces. Core Lean only; the proofs are `decide`, `simp`, `omega` and induction.

  Abstraction.
  * Time is a natural number of seconds. The Python clock is a float; `fwd d` and `back d` move it.
  * Rate: one token per `P` seconds (the default 12/min is P = 5). Tokens are kept in units of 1/P
    token, so a second of refill is one unit, `burst` is `B * P` units and a take is `P` units. This
    is exact for every rate 1/P with whole-second clock steps; `tok` is an Int (a forced take can go
    below 0: watch.py:236).
  * The four parts of RateLimiter touch disjoint fields and share only the clock: the bucket
    (`_tokens`, `_refilled`), the host gap (`_last`), the backoff (`_strikes`, `_until`) and the
    model cap (`_model_day`, `_model_n`). `ready_in` writes only the bucket (through `_refill`);
    `take` writes the bucket and `_last`. So each part is modelled on its own and every trace of the
    class projects onto a trace of each part. The host gap needs no model: `_last` is written only by
    `take` and read only by `ready_in` as `max(0, last + gap - now)`, which can only delay.
  * The backoff dicts are keyed by host and every method touches one key, so one host is modelled.
    `retry_after` is a Nat, 0 standing for None or <= 0 (watch.py:244).
  * The model cap's day label is `now / L`. A clock stepped back across midnight, or the Mac's time
    zone moved west, is a `back` that crosses a multiple of L.
  * Property 1's monotone clock is the largest reading the limiter took (at a `ready_in` or `take`).
    A forward excursion of the clock that no call read is invisible to any limiter; the refill it
    skipped is credited when the clock next passes the mark, which is real elapsed time.
  * Part E (the gate) models only the backoff half of `ready_in` (the half property 5 is about) and
    the check lock `self._lock`; the bucket and the gap can only make `ready_in` larger. A refused
    read's penalty is at least 1 s (backoff_base is 60), applied with the fixed `penalize`.

  Since this model (re-verification, 2026-09-25): `penalize` caps the ladder's exponent at 30 (the same values
  for every base and cap in use, where 2 ** 1024 overflowed); the gate here has one host, and a page watch's
  model calls go to a second one, OpenRouter, so `_model_budget` also refuses a model call while OpenRouter's
  own hold is in force, and OpenRouter's refusals are charged to OpenRouter (`FetchError.host`).
-/
namespace Eve.WatchLimiter

/-! ## A. The token bucket: `_refill`, `ready_in` (its bucket half), `take` -/

structure Bucket where
  now : Nat
  tok : Int              -- `_tokens`, in units of 1/P token
  mark : Nat             -- `_refilled`
  refTok : Int           -- ghost: the reference bucket on the monotone clock
  refClock : Nat         -- ghost: the monotone clock, the largest reading taken so far
  ready : Bool := false  -- ghost: the last `ready_in` said the bucket half is 0, and no take since
  debt : Bool := false   -- ghost: a take made on such a `ready_in` left the bucket below 0
  deriving DecidableEq, Repr

inductive BEv where
  | fwd (d : Nat)        -- the clock moves forward
  | back (d : Nat)       -- the clock steps back (NTP, a sleep/wake correction)
  | ready                -- `ready_in(host)`, watch.py:226-232
  | take                 -- `take(host)`, watch.py:234-237
  deriving DecidableEq, Repr

/-- `__init__`, watch.py:211-212: a full bucket, the refill mark at the first reading. -/
def Bucket.init (P B t : Nat) : Bucket :=
  { now := t, tok := (B * P : Nat), mark := t, refTok := (B * P : Nat), refClock := t }

/-- `_refill`, watch.py:219-224, and the reference bucket's refill on the monotone clock. -/
def refill (P B : Nat) (s : Bucket) : Bucket :=
  { s with
    -- min(burst, tokens + max(0, now - refilled) * rate): Nat subtraction is the max(0, ...)
    tok := min ((B * P : Nat) : Int) (s.tok + ((s.now - s.mark : Nat) : Int)),
    -- refilled = max(refilled, now)
    mark := max s.mark s.now,
    -- the reference: the monotone clock advances to max(M, now) and the bucket gains what it advanced
    refTok := min ((B * P : Nat) : Int) (s.refTok + ((max s.refClock s.now - s.refClock : Nat) : Int)),
    refClock := max s.refClock s.now }

def Bucket.step (P B : Nat) (s : Bucket) : BEv → Bucket
  | .fwd d => { s with now := s.now + d }
  | .back d => { s with now := s.now - d }
  -- ready_in: _refill, then the bucket half is 0 exactly when tokens >= 1.0 (watch.py:229)
  | .ready => let s1 := refill P B s; { s1 with ready := decide ((P : Int) ≤ s1.tok) }
  -- take: _refill, then tokens -= 1.0 (watch.py:235-236). The reference spends the same token.
  | .take => let s1 := refill P B s
      { s1 with tok := s1.tok - P, refTok := s1.refTok - P, ready := false,
                debt := s1.debt || (s.ready && decide (s1.tok - P < 0)) }

def Bucket.run (P B t : Nat) (evs : List BEv) : Bucket := evs.foldl (Bucket.step P B) (Bucket.init P B t)

def BucketSpec (P B : Nat) (s : Bucket) : Bool :=
  decide (s.tok ≤ ((B * P : Nat) : Int)) && decide (s.tok = s.refTok) && !s.debt

def BucketInv (P B : Nat) (s : Bucket) : Prop :=
  s.tok ≤ ((B * P : Nat) : Int) ∧ s.tok = s.refTok ∧ s.mark = s.refClock ∧
  (s.ready = true → (P : Int) ≤ s.tok) ∧ s.debt = false

/-- One `_refill` keeps the bucket equal to the reference, within the burst, and never lowers it. -/
theorem refill_inv (P B : Nat) (s : Bucket) (h1 : s.tok ≤ ((B * P : Nat) : Int)) (h2 : s.tok = s.refTok)
    (h3 : s.mark = s.refClock) :
    (refill P B s).tok ≤ ((B * P : Nat) : Int) ∧ (refill P B s).tok = (refill P B s).refTok ∧
    (refill P B s).mark = (refill P B s).refClock ∧ s.tok ≤ (refill P B s).tok ∧
    (refill P B s).ready = s.ready ∧ (refill P B s).debt = s.debt := by
  have e1 : (refill P B s).tok = min ((B * P : Nat) : Int) (s.tok + ((s.now - s.mark : Nat) : Int)) := rfl
  have e2 : (refill P B s).refTok =
      min ((B * P : Nat) : Int) (s.refTok + ((max s.refClock s.now - s.refClock : Nat) : Int)) := rfl
  have e3 : (refill P B s).mark = max s.mark s.now := rfl
  have e4 : (refill P B s).refClock = max s.refClock s.now := rfl
  have hn : (s.now - s.mark : Nat) = (max s.refClock s.now - s.refClock : Nat) := by omega
  refine ⟨?_, ?_, ?_, ?_, rfl, rfl⟩
  · rw [e1]; omega
  · rw [e1, e2, hn, h2]
  · rw [e3, e4, h3]
  · rw [e1]; omega

theorem bucket_inv_step (P B : Nat) (s : Bucket) (e : BEv) (h : BucketInv P B s) :
    BucketInv P B (Bucket.step P B s e) := by
  obtain ⟨h1, h2, h3, h4, h5⟩ := h
  obtain ⟨r1, r2, r3, r4, r5, r6⟩ := refill_inv P B s h1 h2 h3
  cases e with
  | fwd d => exact ⟨h1, h2, h3, h4, h5⟩
  | back d => exact ⟨h1, h2, h3, h4, h5⟩
  | ready =>
    refine ⟨r1, r2, r3, ?_, r6.trans h5⟩
    intro hr
    exact of_decide_eq_true hr
  | take =>
    refine ⟨?_, ?_, r3, ?_, ?_⟩
    · show (refill P B s).tok - P ≤ _; omega
    · show (refill P B s).tok - P = (refill P B s).refTok - P; rw [r2]
    · intro hr; exact absurd hr (by simp [Bucket.step])
    · show ((refill P B s).debt || (s.ready && decide ((refill P B s).tok - P < 0))) = false
      rw [r6, h5]
      cases hs : s.ready
      · simp
      · have := h4 hs; simp; omega

theorem bucket_inv_run (P B : Nat) (evs : List BEv) :
    ∀ s, BucketInv P B s → BucketInv P B (evs.foldl (Bucket.step P B) s) := by
  induction evs with
  | nil => intro s h; exact h
  | cons e rest ih => intro s h; exact ih _ (bucket_inv_step P B s e h)

/-- Properties 1 and 2, on the code as it is (the bucket needs no fix): for every rate, burst, start
time and trace of clock moves, `ready_in` and `take`, the bucket holds at most `burst`, equals the
reference bucket on the monotone clock, and no take made on a `ready_in` of 0 went into debt. -/
theorem bucket_invariant (P B t : Nat) (evs : List BEv) : BucketSpec P B (Bucket.run P B t evs) = true := by
  have h := bucket_inv_run P B evs (Bucket.init P B t)
    ⟨by simp [Bucket.init], rfl, rfl, by simp [Bucket.init], rfl⟩
  obtain ⟨h1, h2, _, _, h5⟩ := h
  unfold BucketSpec Bucket.run
  rw [h5]
  simp only [decide_eq_true h1, decide_eq_true h2]
  rfl

/-- The same refill without the `max` on the mark (`_refilled = now`) does mint: an empty bucket of two
tokens, forward 5 s, back 5 s, forward 5 s again refills the same five seconds twice (two tokens where
the monotone clock gives one); the real `_refill` gives one. The `max` at watch.py:224 is what property 1 rests on. -/
def refillNoMax (P B : Nat) (s : Bucket) : Bucket :=
  { refill P B s with mark := s.now }

theorem without_the_max_a_wiggle_mints :
    let P := 5; let B := 2
    let s0 : Bucket := { Bucket.init P B 0 with tok := 0, refTok := 0 }   -- an empty bucket at 0
    -- without the max: forward 5 s (+1 token), back to 0, the same 5 s again (+1 more)
    let n3 := refillNoMax P B { refillNoMax P B { refillNoMax P B { s0 with now := 5 } with now := 0 } with now := 5 }
    -- the real `_refill` on the same readings
    let r3 := refill P B { refill P B { refill P B { s0 with now := 5 } with now := 0 } with now := 5 }
    n3.tok = 10 ∧ r3.tok = 5 ∧ r3.tok = r3.refTok := by
  decide

/-! ## B. The backoff: `penalize`, `clear`, `backed_off` (one host) -/

structure Hold where
  now : Nat := 0
  strikes : Nat := 0     -- `_strikes.get(host, 0)`
  holdEnd : Nat := 0     -- `_until.get(host, 0.0)`
  need : Nat := 0        -- ghost: the latest end any penalty since the last clear asked for
  lastLadder : Nat := 0  -- ghost: the ladder step of the last penalty since the clear (0: none)
  pens : Nat := 0        -- ghost: penalties since the last clear
  bad : Bool := false    -- ghost: a penalty broke a bound, or the ladder shrank
  deriving DecidableEq, Repr

inductive HEv where
  | fwd (d : Nat)
  | back (d : Nat)
  | pen (ra : Nat)       -- `penalize(host, retry_after)`; ra = 0 is None or <= 0
  | clear                -- `clear(host)`, watch.py:249-251 (a success, watch.py:1452)
  deriving DecidableEq, Repr

/-- The ladder step of the next strike: min(backoff_max, backoff_base * 2 ** (strikes - 1)), with
`strikes` already incremented (watch.py:241-243). `k` is the count before the increment. -/
def ladder (base mx k : Nat) : Nat := min mx (base * 2 ^ k)

/-- What `penalize` returns, watch.py:243-245 (the same before and after the fix). -/
def secs (base mx k ra : Nat) : Nat :=
  if ra > 0 then max (ladder base mx k) (min mx ra) else ladder base mx k

/-- The per-call obligations of property 3 (bounds, ladder never shrinks, a clear restarts it). -/
def penOk (base mx : Nat) (s : Hold) (ra : Nat) : Bool :=
  decide (secs base mx s.strikes ra ≤ mx) && decide (min ra mx ≤ secs base mx s.strikes ra) &&
  decide (s.lastLadder ≤ ladder base mx s.strikes) &&
  (s.pens != 0 || decide (ladder base mx s.strikes = min mx base))

/-- One step of either version; `fix` picks watch.py:246 as it is or as fixed. -/
def Hold.step (fix : Bool) (base mx : Nat) (s : Hold) : HEv → Hold
  | .fwd d => { s with now := s.now + d }
  | .back d => { s with now := s.now - d }
  | .pen ra =>
      let v := secs base mx s.strikes ra
      { s with strikes := s.strikes + 1,
               -- current: `self._until[host] = self._clock() + secs` (watch.py:246)
               -- fixed:   `self._until[host] = max(self._until.get(host, 0.0), self._clock() + secs)`
               holdEnd := if fix then max s.holdEnd (s.now + v) else s.now + v,
               need := max s.need (s.now + v),
               lastLadder := ladder base mx s.strikes, pens := s.pens + 1,
               bad := s.bad || !penOk base mx s ra }
  | .clear => { s with strikes := 0, holdEnd := 0, need := 0, lastLadder := 0, pens := 0 }

def Hold.run (fix : Bool) (base mx : Nat) (evs : List HEv) : Hold := evs.foldl (Hold.step fix base mx) {}

/-- `backed_off(host)`, watch.py:253-254. -/
def Hold.backedOff (s : Hold) : Nat := s.holdEnd - s.now

def HoldSpec (s : Hold) : Bool := !s.bad && decide (s.need ≤ s.holdEnd)

/-- Property 3's hold half, broken. Retry-After 3000 at t = 0; ten seconds later a read that was let
through before that answer is refused again with no Retry-After (strike 2: 120 s). The hold now ends
at 130, not 3000: `backed_off` says 120 where the server asked for 2990 more. -/
theorem current_violates_backoff :
    let s := Hold.run false 60 3600 [.pen 3000, .fwd 10, .pen 0]
    s.backedOff = 120 ∧ s.need = 3000 ∧ HoldSpec s = false := by
  decide

/-- The returned value may drop below an earlier Retry-After (3000, then 120): by design, the ladder is
counted in strikes and a Retry-After is a floor for its own answer only (urllib3's shape, and pinned by
test_retry_after_holds_the_host_and_backoff_doubles_capped). Not a violation: the bounds hold. -/
theorem returned_value_follows_the_ladder :
    let s1 := Hold.run false 60 3600 [.pen 3000]
    secs 60 3600 0 3000 = 3000 ∧ secs 60 3600 s1.strikes 0 = 120 ∧ s1.bad = false := by
  decide

def HoldInv (fix : Bool) (base mx : Nat) (s : Hold) : Prop :=
  s.bad = false ∧ s.strikes = s.pens ∧
  s.lastLadder = (if s.strikes = 0 then 0 else ladder base mx (s.strikes - 1)) ∧
  (fix = true → s.need ≤ s.holdEnd)

theorem ladder_mono (base mx k : Nat) : ladder base mx k ≤ ladder base mx (k + 1) := by
  unfold ladder
  have : base * 2 ^ k ≤ base * 2 ^ (k + 1) :=
    Nat.mul_le_mul_left base (Nat.pow_le_pow_right (by decide) (Nat.le_succ k))
  omega

theorem hold_inv_step (fix : Bool) (base mx : Nat) (s : Hold) (e : HEv) (h : HoldInv fix base mx s) :
    HoldInv fix base mx (Hold.step fix base mx s e) := by
  obtain ⟨h1, h2, h3, h4⟩ := h
  cases e with
  | fwd d => exact ⟨h1, h2, h3, h4⟩
  | back d => exact ⟨h1, h2, h3, h4⟩
  | clear => exact ⟨h1, rfl, by simp [Hold.step], fun _ => by simp [Hold.step]⟩
  | pen ra =>
    refine ⟨?_, by simp [Hold.step, h2], by simp [Hold.step], ?_⟩
    · simp only [Hold.step, h1, Bool.false_or, Bool.not_eq_false']
      have hl : s.lastLadder ≤ ladder base mx s.strikes := by
        rw [h3]; split
        · omega
        · have := ladder_mono base mx (s.strikes - 1)
          rwa [Nat.sub_add_cancel (by omega)] at this
      have hz : s.pens = 0 → ladder base mx s.strikes = min mx base := by
        intro hp; rw [← h2] at hp; simp [ladder, hp]
      unfold penOk secs
      by_cases hp : s.pens = 0
      · have := hz hp
        split <;> simp [hp, this] <;> unfold ladder at * <;> omega
      · split <;> simp [hp] <;> unfold ladder at * <;> omega
    · intro hf; subst hf; simp only [Hold.step, ite_true]; have := h4 rfl; omega

theorem hold_inv_run (fix : Bool) (base mx : Nat) (evs : List HEv) :
    ∀ s, HoldInv fix base mx s → HoldInv fix base mx (evs.foldl (Hold.step fix base mx) s) := by
  induction evs with
  | nil => intro s h; exact h
  | cons e rest ih => intro s h; exact ih _ (hold_inv_step fix base mx s e h)

/-- Property 3's bounds hold on the code as it is, for every base, cap and trace: no penalty returns
more than backoff_max or less than min(retry_after, backoff_max), the ladder never shrinks between
clears, strikes count the penalties since the clear, and the first one after a clear is the base. -/
theorem current_penalty_bounds (base mx : Nat) (evs : List HEv) :
    (Hold.run false base mx evs).bad = false ∧
    (Hold.run false base mx evs).strikes = (Hold.run false base mx evs).pens :=
  let h := hold_inv_run false base mx evs {} ⟨rfl, rfl, by simp, by simp⟩
  ⟨h.1, h.2.1⟩

theorem hold_fixed (base mx : Nat) (evs : List HEv) : HoldSpec (Hold.run true base mx evs) = true := by
  obtain ⟨h1, _, _, h4⟩ := hold_inv_run true base mx evs {} ⟨rfl, rfl, by simp, by simp⟩
  have h5 := h4 rfl
  simp [HoldSpec, Hold.run, h1, h5]

/-! ## C. The daily model cap: `_roll_day`, `model_ok`, `model_used` -/

/-- A dict as a total function with default 0. -/
def upd (f : Nat → Nat) (k v : Nat) : Nat → Nat := fun d => if d = k then v else f d

structure Cap where
  now : Nat := 0
  mday : Option Nat := none            -- `_model_day` ("" at start is `none`)
  mn : Nat := 0                        -- `_model_n`
  seen : Nat → Nat := fun _ => 0       -- the fix's `_model_seen` (never written by the current code)
  used : Nat → Nat := fun _ => 0       -- ghost: `model_used` calls per day label
  bad : Bool := false                  -- ghost: a `model_ok` answered against the day's count

inductive CEv where
  | fwd (d : Nat)
  | back (d : Nat)
  | ok                                 -- `model_ok()`, watch.py:261-263 (`_model_budget`, 1256-1258)
  | used                               -- `model_used()`, watch.py:265-267
  deriving DecidableEq, Repr

/-- `_roll_day`. Current (watch.py:256-259): a label unlike the stored one resets the count to 0.
Fixed: the day being left is remembered, and a label seen before gets its count back. -/
def roll (fix : Bool) (L : Nat) (s : Cap) : Cap :=
  let today := s.now / L
  if s.mday = some today then s
  else if fix then
    let seen1 := match s.mday with
      | some d => upd s.seen d s.mn        -- self._model_seen[self._model_day] = self._model_n
      | none => s.seen
    -- self._model_day, self._model_n = today, self._model_seen.pop(today, 0)
    { s with mday := some today, mn := seen1 today, seen := upd seen1 today 0 }
  else { s with mday := some today, mn := 0 }

def Cap.step (fix : Bool) (cap L : Nat) (s : Cap) : CEv → Cap
  | .fwd d => { s with now := s.now + d }
  | .back d => { s with now := s.now - d }
  | .ok => let s1 := roll fix L s
      { s1 with bad := s1.bad || (decide (s1.mn < cap) != decide (s.used (s.now / L) < cap)) }
  | .used => let s1 := roll fix L s
      { s1 with mn := s1.mn + 1, used := upd s.used (s.now / L) (s.used (s.now / L) + 1) }

def Cap.run (fix : Bool) (cap L t : Nat) (evs : List CEv) : Cap := evs.foldl (Cap.step fix cap L) { now := t }

/-- What `model_ok()` would answer now, and whether that is today's count against the cap. -/
def Cap.answerRight (fix : Bool) (cap L : Nat) (s : Cap) : Bool :=
  decide ((roll fix L s).mn < cap) == decide (s.used (s.now / L) < cap)

def CapSpec (fix : Bool) (cap L : Nat) (s : Cap) : Bool := !s.bad && s.answerRight fix cap L

/-- Property 4, broken. Cap 2, a day of 10 s: both calls spent at t = 9 (day 0); the clock steps to
10 (day 1) and `model_ok` rolls to day 1; then back to 9, day 0 again. Day 0 spent its two calls, yet
`model_ok` says yes: the day was rolled as if new. -/
theorem current_violates_model_cap :
    let s := Cap.run false 2 10 9 [.used, .used, .fwd 1, .ok, .back 1, .ok]
    s.used 0 = 2 ∧ s.mn = 0 ∧ s.bad = true ∧ CapSpec false 2 10 s = false := by
  decide

def CapInv (_L : Nat) (s : Cap) : Prop :=
  s.bad = false ∧ ∀ d, s.used d = (if s.mday = some d then s.mn else s.seen d)

theorem roll_keeps (fix : Bool) (L : Nat) (s : Cap) :
    (roll fix L s).used = s.used ∧ (roll fix L s).now = s.now ∧ (roll fix L s).bad = s.bad := by
  unfold roll
  by_cases ht : s.mday = some (s.now / L)
  · simp [ht]
  · cases fix <;> simp [ht]

theorem roll_fix_count (L : Nat) (s : Cap) (h : ∀ d, s.used d = (if s.mday = some d then s.mn else s.seen d)) :
    (roll true L s).mday = some (s.now / L) ∧ (roll true L s).mn = s.used (s.now / L) ∧
    (roll true L s).bad = s.bad ∧
    ∀ d, s.used d = (if (roll true L s).mday = some d then (roll true L s).mn else (roll true L s).seen d) := by
  unfold roll
  by_cases ht : s.mday = some (s.now / L)
  · rw [ite_eq_left ht]
    refine ⟨ht, ?_, rfl, fun d => h d⟩
    rw [h]; simp [ht]
  · rw [ite_eq_right ht, ite_eq_left rfl]
    cases hm : s.mday with
    | none =>
      refine ⟨rfl, ?_, rfl, ?_⟩
      · rw [h, hm]; simp
      · intro d; rw [h, hm]
        by_cases hd : d = s.now / L
        · subst hd; simp
        · simp [upd, hd, Ne.symm hd]
    | some d0 =>
      have h0 : d0 ≠ s.now / L := fun e => ht (by rw [hm, e])
      refine ⟨rfl, ?_, rfl, ?_⟩
      · rw [h, hm]; simp [upd, h0, Ne.symm h0]
      · intro d; rw [h, hm]
        by_cases hd : d = s.now / L
        · subst hd; simp [upd, h0, Ne.symm h0]
        · by_cases hd0 : d = d0
          · subst hd0; simp [upd, hd, Ne.symm hd]
          · simp [upd, hd, Ne.symm hd, hd0, Ne.symm hd0]

theorem cap_inv_step (cap L : Nat) (s : Cap) (e : CEv) (h : CapInv L s) : CapInv L (Cap.step true cap L s e) := by
  obtain ⟨h1, h2⟩ := h
  cases e with
  | fwd d => exact ⟨h1, h2⟩
  | back d => exact ⟨h1, h2⟩
  | ok =>
    obtain ⟨_, hm, hb, hr⟩ := roll_fix_count L s h2
    obtain ⟨hu, _, _⟩ := roll_keeps true L s
    refine ⟨?_, ?_⟩
    · simp [Cap.step, hb, h1, hm]
    · intro d
      show (roll true L s).used d =
        if (roll true L s).mday = some d then (roll true L s).mn else (roll true L s).seen d
      rw [hu]; exact hr d
  | used =>
    obtain ⟨hd, hm, hb, hr⟩ := roll_fix_count L s h2
    refine ⟨?_, ?_⟩
    · simp [Cap.step, hb, h1]
    · intro d
      simp only [Cap.step, upd, hd]
      by_cases hdt : d = s.now / L
      · subst hdt; simp [hm]
      · have := hr d
        rw [hd] at this
        simp [hdt, Ne.symm hdt] at this ⊢
        exact this

theorem cap_inv_run (cap L : Nat) (evs : List CEv) :
    ∀ s, CapInv L s → CapInv L (evs.foldl (Cap.step true cap L) s) := by
  induction evs with
  | nil => intro s h; exact h
  | cons e rest ih => intro s h; exact ih _ (cap_inv_step cap L s e h)

theorem cap_fixed (cap L t : Nat) (evs : List CEv) : CapSpec true cap L (Cap.run true cap L t evs) = true := by
  have h := cap_inv_run cap L evs { now := t } ⟨rfl, fun d => by simp⟩
  obtain ⟨h1, h2⟩ := h
  obtain ⟨_, hm, _, _⟩ := roll_fix_count L _ h2
  unfold CapSpec Cap.answerRight Cap.run
  rw [h1, hm]
  simp

/-! ## E. The gate in front of every read (Watcher.tick, Watcher.add) -/

inductive Ph where
  | idle                 -- not checking
  | gated                -- let through `ready_in` and `take`, waiting on `self._lock`
  | reading              -- holds the lock; its request has gone to the host
  deriving DecidableEq, Repr

structure Gate where
  now : Nat := 0
  holdEnd : Nat := 0     -- the host's hold (`_until`), with the fixed `penalize`
  a : Ph := .idle        -- two checkers on one host: tick's `_run_one` and `add`'s first check
  b : Ph := .idle
  sentHeld : Bool := false   -- ghost: a request went to the host while it was held
  deriving DecidableEq, Repr

inductive GEv where
  | fwd (d : Nat)
  | gate (i : Bool)      -- tick 1498-1502 / add 1638-1649: ready_in(host) == 0, then take(host)
  | start (i : Bool)     -- `async with self._lock` acquired (_run_one 1475 / add 1650), the read goes
  | refuse (i : Bool) (ra : Nat)   -- the read comes back 429/503: penalize (check, 1445-1446)
  | done (i : Bool)      -- the read comes back fine, or the check ends; the lock is released
  deriving DecidableEq, Repr

def Gate.ph (s : Gate) (i : Bool) : Ph := if i then s.a else s.b
def Gate.set (s : Gate) (i : Bool) (p : Ph) : Gate := if i then { s with a := p } else { s with b := p }
def Gate.locked (s : Gate) : Bool := s.a == .reading || s.b == .reading

def Gate.step (fix : Bool) (s : Gate) : GEv → Gate
  | .fwd d => { s with now := s.now + d }
  -- the backoff half of ready_in is 0 exactly when now >= holdEnd
  | .gate i => if s.ph i == .idle && decide (s.holdEnd ≤ s.now) then s.set i .gated else s
  | .start i =>
      if s.ph i == .gated && !s.locked then
        -- fixed: under the lock, `backed_off(host) > 0` pushes the watch back instead of reading
        if fix && decide (s.now < s.holdEnd) then s.set i .idle
        else { s.set i .reading with sentHeld := s.sentHeld || decide (s.now < s.holdEnd) }
      else s
  | .refuse i ra =>
      if s.ph i == .reading then { s.set i .idle with holdEnd := max s.holdEnd (s.now + max ra 1) } else s
  | .done i => if s.ph i == .reading then s.set i .idle else s

def Gate.run (fix : Bool) (evs : List GEv) : Gate := evs.foldl (Gate.step fix) {}

def GateSpec (s : Gate) : Bool := !s.sentHeld

/-- Property 5, broken: `add` (a) takes and reads while tick's watch (b) on the same host is due;
the host gap passes, tick lets b through and b waits on the lock; a's read comes back 429 with
Retry-After 3000; the lock passes to b, whose read goes to the host with 3000 s of its hold left. -/
theorem current_violates_gate :
    let s := Gate.run false [.gate true, .start true, .fwd 25, .gate false, .fwd 10, .refuse true 3000,
                             .start false]
    s.b = .reading ∧ s.holdEnd = 3035 ∧ s.sentHeld = true ∧ GateSpec s = false := by
  decide

theorem gate_inv_step (s : Gate) (e : GEv) (h : s.sentHeld = false) : (Gate.step true s e).sentHeld = false := by
  cases e with
  | fwd d => exact h
  | gate i => simp only [Gate.step]; split <;> cases i <;> simp [Gate.set, h]
  | start i =>
    simp only [Gate.step]
    split
    · by_cases hl : s.now < s.holdEnd
      · simp only [hl, decide_true, Bool.true_and, ite_true]; cases i <;> simp [Gate.set, h]
      · simp only [hl, decide_false, Bool.and_false, Bool.false_eq_true, ite_false]
        cases i <;> simp [h]
    · exact h
  | refuse i ra => simp only [Gate.step]; split <;> cases i <;> simp [Gate.set, h]
  | done i => simp only [Gate.step]; split <;> cases i <;> simp [Gate.set, h]

theorem gate_fixed (evs : List GEv) : GateSpec (Gate.run true evs) = true := by
  have : ∀ (l : List GEv) (s : Gate), s.sentHeld = false → (l.foldl (Gate.step true) s).sentHeld = false := by
    intro l; induction l with
    | nil => intro s h; exact h
    | cons e rest ih => intro s h; exact ih _ (gate_inv_step s e h)
  simp [GateSpec, Gate.run, this evs {} rfl]

/-! ## The two shapes check-all asks for -/

/-- The current code breaks the Spec on three concrete traces: a hold shortened (B), a spent day's cap
given back (C), a read sent inside a hold (E). -/
theorem current_violates :
    HoldSpec (Hold.run false 60 3600 [.pen 3000, .fwd 10, .pen 0]) = false ∧
    CapSpec false 2 10 (Cap.run false 2 10 9 [.used, .used, .fwd 1, .ok, .back 1, .ok]) = false ∧
    GateSpec (Gate.run false [.gate true, .start true, .fwd 25, .gate false, .fwd 10, .refuse true 3000,
                              .start false]) = false :=
  ⟨current_violates_backoff.2.2, current_violates_model_cap.2.2.2, current_violates_gate.2.2.2⟩

/-- The fixed code keeps every property over every trace, for every parameter: the bucket (unchanged),
the backoff with the `max` on `_until`, the model cap with the remembered days, the gate re-checked
under the lock. -/
theorem fixed_invariant :
    (∀ P B t (evs : List BEv), BucketSpec P B (Bucket.run P B t evs) = true) ∧
    (∀ base mx (evs : List HEv), HoldSpec (Hold.run true base mx evs) = true) ∧
    (∀ cap L t (evs : List CEv), CapSpec true cap L (Cap.run true cap L t evs) = true) ∧
    (∀ evs : List GEv, GateSpec (Gate.run true evs) = true) :=
  ⟨bucket_invariant, hold_fixed, cap_fixed, gate_fixed⟩

end Eve.WatchLimiter
