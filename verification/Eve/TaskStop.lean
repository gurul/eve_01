/-
  Stopping a computer task from the Telegram chat (bridge/src/eve_bridge/telegram.py).

  `_run_agent` awaits `agent.run(goal)`; once it returns, the task (an asyncio Task) is still not
  done: it awaits `_close_progress`, then the `_say` of the result, and maybe `_send_screen`. So
  `task_running` stays true for that whole tail. The property:

    if the agent's run returned a real result (no cancel reached it first), the owner receives that
    result and is never told "Stopped."; and a stop that did reach the running run says "Stopped."
    and the result (the agent's own "I stopped") is not sent as well.

  Abstraction. `ret` is `agent.run` returning. By the agent contract (FakeAgent, chrome_lane.py:128,
  app_reflex.py:323) a cancel that reaches a run before it returns makes it return its stopped
  result, so "completed" is "returned with no cancel before it". A cancel after the return is a
  no-op on the agent. The awaits of the tail are `step`s: `closing` is the `_close_progress` await,
  `delivering` the result's `_say` and the optional screen. `stop` is "stop" typed (`_handle`,
  telegram.py:1861-1866 before the fix, -> `_stop`); `toolStop` is the model's stop_task tool. The
  Stop button (`_stop_task`) checks `_task_progress is progress`, which `_run_agent` clears before
  its first await (telegram.py:3327 before the fix), so after the return it already reached nothing;
  before the return it is the same as `stop`.
-/
namespace Eve.TaskStop

inductive Phase where
  | running        -- awaiting agent.run
  | closing        -- awaiting _close_progress
  | delivering     -- awaiting the result's _say (and the screen)
  | over           -- the task is done: task_running is false
  deriving DecidableEq, Repr

inductive Event where
  | stop
  | toolStop
  | ret
  | step
  deriving DecidableEq, Repr

structure S where
  phase : Phase := .running
  stopped : Bool := false      -- _stopped_from_chat
  cancelled : Bool := false    -- agent.cancel reached the run before it returned
  returned : Bool := false     -- _task_returned (the fix; never set by the current code)
  completed : Bool := false    -- ghost: the run returned a real result
  interrupted : Bool := false  -- ghost: a typed stop reached the run before it returned
  toldStopped : Bool := false  -- STOPPED_LINE was said
  finalSent : Bool := false    -- the result was sent
  deriving DecidableEq, Repr

def Spec (s : S) : Bool :=
  (!s.completed || !s.toldStopped) &&
  (!(s.completed && s.phase == .over) || s.finalSent) &&
  (!s.interrupted || (s.toldStopped && !s.finalSent))

def taskRunning (s : S) : Bool := s.phase != .over          -- task_running, telegram.py:1501

/-- The tail of `_run_agent` after `agent.run` returns, the same before and after the fix. -/
def tail (s : S) : S :=
  match s.phase with
  -- the `_close_progress` await ends; `if not self._stopped_from_chat:` (telegram.py:3329-3330 before
  -- the fix, 3347-3348 after): the result goes, or nothing does.
  | .closing => if s.stopped then { s with phase := .over }
                else { s with finalSent := true, phase := .delivering }
  -- the result's `_say` and the screen are done (telegram.py:3333-3338 before, 3351-3356 after)
  | .delivering => { s with phase := .over }
  | _ => s

/-! ## The current code -/

def Cur.step (s : S) : Event → S
  -- _stop, telegram.py:2896-2902: only `task_running` is checked.
  | .stop => if taskRunning s then
      { s with stopped := true, toldStopped := true,
               cancelled := s.cancelled || s.phase == .running,
               interrupted := s.interrupted || s.phase == .running }
    else s                                         -- telegram.py:2903-2904: "Nothing is running."
  -- _tool_stop_task, telegram.py:3196-3199: cancels, sets no flag, says nothing itself.
  | .toolStop => if taskRunning s then { s with cancelled := s.cancelled || s.phase == .running } else s
  -- agent.run returns, telegram.py:3314; the next await is _close_progress at 3329.
  | .ret => if s.phase == .running then { s with phase := .closing, completed := !s.cancelled } else s
  | .step => tail s

def Cur.run (evs : List Event) : S := evs.foldl Cur.step {}

/-- The run returns its result, then "stop" is typed while the progress message closes: the owner
is told "Stopped." and never gets the result. -/
theorem current_violates :
    let s := Cur.run [.ret, .stop, .step]
    s.completed = true ∧ s.phase = .over ∧ s.toldStopped = true ∧ s.finalSent = false ∧
      Spec s = false := by
  decide

/-! ## The fixed code -/

def Fix.step (s : S) : Event → S
  -- _stop, telegram.py:2913-2919 (fixed): also `not self._task_returned`.
  | .stop => if taskRunning s && !s.returned then
      { s with stopped := true, toldStopped := true,
               cancelled := s.cancelled || s.phase == .running,
               interrupted := s.interrupted || s.phase == .running }
    else s                                         -- telegram.py:2920-2921: "Nothing is running."
  -- _tool_stop_task, telegram.py:3213-3216 (fixed): the same guard.
  | .toolStop => if taskRunning s && !s.returned then
      { s with cancelled := s.cancelled || s.phase == .running } else s
  -- agent.run returns; telegram.py:3341 sets `_task_returned` before the first await (3347).
  | .ret => if s.phase == .running then
      { s with phase := .closing, completed := !s.cancelled, returned := true } else s
  | .step => tail s

def Fix.run (evs : List Event) : S := evs.foldl Fix.step {}

/-! ### Proof: an inductive invariant, checked on every state and event. -/

/-- The strengthened invariant. -/
def Inv (s : S) : Bool :=
  Spec s &&
  (s.stopped == s.interrupted) && (s.toldStopped == s.stopped) &&
  (!s.stopped || s.cancelled) &&
  (!s.completed || !s.cancelled) &&
  (if s.phase == .running then !s.returned && !s.completed && !s.finalSent else s.returned) &&
  (!s.finalSent || !s.stopped) &&
  (s.phase == .running || s.completed == !s.cancelled) &&
  (s.phase != .delivering || s.finalSent) &&
  (s.phase != .closing || !s.finalSent) &&
  (s.phase != .over || s.completed || s.stopped || s.cancelled)

theorem inv_step : ∀ (s : S) (e : Event), Inv s = true → Inv (Fix.step s e) = true := by
  intro s e
  rcases s with ⟨ph, a, b, c, d, f, g, h⟩
  cases ph <;> cases a <;> cases b <;> cases c <;> cases d <;> cases f <;> cases g <;> cases h <;>
    cases e <;> decide

theorem inv_run (evs : List Event) : ∀ s : S, Inv s = true → Inv (evs.foldl Fix.step s) = true := by
  induction evs with
  | nil => intro s h; exact h
  | cons e rest ih => intro s h; exact ih _ (inv_step s e h)

/-- For every trace of stops, tool stops, the run's return and the tail's awaits: a completed run's
result reaches the owner and "Stopped." is never said for it; a stop that reached the run says
"Stopped." and no result follows. -/
theorem fixed_invariant (evs : List Event) : Spec (Fix.run evs) = true := by
  have h := inv_run evs {} (by decide)
  have spec_of_inv : ∀ s : S, Inv s = true → Spec s = true := by
    intro s; unfold Inv; cases Spec s <;> simp
  exact spec_of_inv _ h

/-- The trace that breaks the current code, on the fixed one: the result is sent. -/
theorem fixed_replays_counterexample :
    let s := Fix.run [.ret, .stop, .step, .step]
    s.finalSent = true ∧ s.toldStopped = false := by
  decide

end Eve.TaskStop
