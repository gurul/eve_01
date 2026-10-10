/-
  A "new claude" that joins the session it opens, in the Telegram inlet (bridge/src/eve_bridge/telegram.py).

  "claude on" with nothing running, or the picker's "new claude", sets `_join_after_launch`: once the terminal
  opens, the chat joins it (`_launch_step` → `_relay_to`). The chat can also be handed to Codex
  (`self._codex_chat`). The two relays are separate fields, so each switch must turn the other off. The
  properties:

    (P1) the Claude and Codex relays are never both on, after any sequence of switches;
    (P2) after "claude off", the Claude relay is off until a new "claude on" (a late join never revives it).

  Abstraction. Each relay is a Bool. `join` is `_join_after_launch`. Opening the terminal is two events, because
  `_launch_step` reads `join` and then awaits `self._launcher` for seconds: `launchStart` takes the flag into
  `opening`, and `launchDone` is the await returning. `stale` stands for "the Claude epoch moved since
  launchStart" (`self._claude_epoch`, compared after the await in the fixed code). `offSinceOn` is a ghost for
  P2: set by "claude off", cleared by "claude on". Which words make which switch is left out.

  The bug, found 2026-10-06. `_launch_step` took the flag, awaited the launcher, then joined without looking
  again. `codex <folder>` in that window turned Claude off and Codex on; then the join turned Claude on: both
  relays on. "claude off" in the window was undone the same way. The fix: the join happens only if the Claude
  epoch is unchanged since the launch began, and the Codex switch moves the epoch (the CLAUDE_ON/OFF branch
  already did).

  Bound: the step lemma is checked by `decide` on all 2^6 = 64 states and 8 events; `fixed_invariant` then holds
  for every trace of any length, by induction.
-/
namespace Eve.RelayJoin

structure S where
  claude : Bool := false
  codex : Bool := false
  join : Bool := false        -- `_join_after_launch`
  opening : Bool := false     -- `_launch_step` holds `join = True` across `await self._launcher(...)`
  stale : Bool := false       -- the Claude epoch moved since the launch began
  offSinceOn : Bool := false  -- ghost: "claude off" came after the last "claude on"
  deriving DecidableEq, Repr

inductive Event where
  | claudeOnJoin    -- "claude on" with a session running: `_claude_on` → `_join` → `_relay_to`
  | claudeOnLaunch  -- "claude on" with none: `_start_then_join`, `join := True`
  | claudeOff
  | codexOn         -- `codex <folder>` / `codex use <folder>`
  | codexOff
  | launchStart     -- `_launch_step` reads and clears `_join_after_launch`, then awaits the launcher
  | launchDone      -- the await returns: join if it was taken (and, fixed, the epoch has not moved)
  | other           -- anything else: messages, questions, images
  deriving DecidableEq, Repr

def Spec (s : S) : Bool := !(s.claude && s.codex) && !(s.offSinceOn && s.claude)

def step (fixed : Bool) (s : S) : Event → S
  -- the CLAUDE_ON / CLAUDE_OFF branch bumps `_claude_epoch`, and "claude on" disconnects Codex
  | .claudeOnJoin => { s with claude := true, codex := false, stale := true, offSinceOn := false }
  | .claudeOnLaunch => { s with claude := false, codex := false, join := true, stale := true, offSinceOn := false }
  | .claudeOff => { s with claude := false, join := false, stale := true, offSinceOn := true }
  -- `codex on`: Claude off; fixed: the Claude epoch moves too
  | .codexOn => { s with codex := true, claude := false, join := false, stale := if fixed then true else s.stale }
  | .codexOff => { s with codex := false }
  | .launchStart => if s.join then { s with opening := true, stale := false, join := false } else s
  | .launchDone =>
      if s.opening && (!fixed || !s.stale) then { s with claude := true, opening := false }
      else { s with opening := false }
  | .other => s

/-! ## The current code -/

def Cur.run (evs : List Event) : S := evs.foldl (step false) {}

/-- "claude on" with nothing running, the terminal opening, `codex <folder>` meanwhile: both relays on. -/
theorem current_violates :
    let s := Cur.run [.claudeOnLaunch, .launchStart, .codexOn, .launchDone]
    s.claude = true ∧ s.codex = true ∧ Spec s = false := by
  decide

/-- The same window with "claude off": the relay comes back on when the terminal opens. -/
theorem current_violates_off :
    let s := Cur.run [.claudeOnLaunch, .launchStart, .claudeOff, .launchDone]
    s.claude = true ∧ Spec s = false := by
  decide

/-! ## The fixed code -/

def Fix.run (evs : List Event) : S := evs.foldl (step true) {}

/-- The inductive strengthening: a join still to come (pending, or opening with the epoch unmoved) has Codex off
and no "claude off" since, because every switch that turns Codex on or Claude off clears `join` and moves the
epoch. -/
def Inv (s : S) : Bool :=
  Spec s && (!s.join || (!s.codex && !s.offSinceOn)) &&
    (!(s.opening && !s.stale) || (!s.codex && !s.offSinceOn))

theorem inv_step : ∀ (s : S) (e : Event), Inv s = true → Inv (step true s e) = true := by
  intro s e
  rcases s with ⟨a, b, c, d, f, g⟩
  cases a <;> cases b <;> cases c <;> cases d <;> cases f <;> cases g <;> cases e <;> decide

theorem inv_run (evs : List Event) : ∀ s : S, Inv s = true → Inv (evs.foldl (step true) s) = true := by
  induction evs with
  | nil => intro s h; exact h
  | cons e rest ih => intro s h; exact ih _ (inv_step s e h)

/-- For every trace of switches and launches: Claude and Codex are never both on, and "claude off" holds until
the next "claude on". -/
theorem fixed_invariant (evs : List Event) : Spec (Fix.run evs) = true := by
  have h := inv_run evs {} (by decide)
  simp only [Inv, Bool.and_eq_true] at h
  exact h.1.1

/-- The traces that break the current code, on the fixed one: the late join is dropped, the switch wins. -/
theorem fixed_replays_counterexample :
    (Fix.run [.claudeOnLaunch, .launchStart, .codexOn, .launchDone]).claude = false ∧
      (Fix.run [.claudeOnLaunch, .launchStart, .claudeOff, .launchDone]).claude = false := by
  decide

/-- The fix keeps the join when nothing switched while the terminal opened. -/
theorem fixed_still_joins :
    (Fix.run [.claudeOnLaunch, .launchStart, .launchDone]).claude = true := by
  decide

end Eve.RelayJoin
