/-
  The question slot of the Telegram inlet (bridge/src/eve_bridge/telegram.py).

  Questions to the owner come from `_ask_user` (a task, an app consent, Codex) and from
  `decide_permission` (a Claude permission prompt). A typed reply goes only to the question named by
  the slot `_pending_answer` (`_answers_pending`, `_handle`). So the property that matters is:

    whenever a question is live, the slot names the most recently opened live question.

  Questions are numbered in the order they open, so "most recently opened" is "largest number".

  Abstraction. A question is *live* from its open until its future settles (typed answer, tap,
  timeout, or its asker cancelled). Settling and the asker's `finally` are separate events, since the
  asker resumes only on a later loop step (and on a timeout it first awaits a `_say`). Line numbers
  are those of the file BEFORE the fix for the current model and AFTER it for the fixed model; the
  slot's four fields (future, chat, strict, words) are one value here, because both versions write
  them together.
-/
namespace Eve.Questions

/-- An event. `ask` is `_ask_user` opening a question, `perm` is `decide_permission` opening one,
`settle i` settles question `i`'s future, `finish i` runs question `i`'s `finally` block. -/
inductive Event where
  | ask
  | perm
  | settle (i : Nat)
  | finish (i : Nat)
  deriving DecidableEq, Repr

/-- The largest element, or `none` for the empty list. -/
def maxOf : List Nat → Option Nat
  | [] => none
  | a :: l => some (match maxOf l with
      | none => a
      | some m => max a m)

/-- The property: when any question is live, the slot names the most recently opened one. -/
def Spec (live : List Nat) (slot : Option Nat) : Prop :=
  live ≠ [] → slot = maxOf live

instance (live : List Nat) (slot : Option Nat) : Decidable (Spec live slot) := by
  unfold Spec; infer_instance

/-! ## The current code -/

structure Cur where
  next : Nat := 0
  live : List Nat := []                     -- opened, future not settled
  waiting : List Nat := []                  -- opened, `finally` not yet run
  slot : Option Nat := none                 -- `_pending_answer`
  before : List (Nat × Option Nat) := []    -- each `_ask_user`'s saved `before[0]`
  perms : List Nat := []                    -- the questions `decide_permission` opened
  deriving Repr

/-- `_awaiting_answer` (telegram.py:1504-1507): the slot is a future not yet done. -/
def Cur.awaiting (s : Cur) : Bool :=
  match s.slot with
  | some j => s.live.contains j
  | none => false

def Cur.step (s : Cur) : Event → Cur
  -- _ask_user, telegram.py:3392-3394: `before` is the slot as it was, then the slot is the new future.
  | .ask => { s with next := s.next + 1, live := s.live ++ [s.next], waiting := s.waiting ++ [s.next],
                     slot := some s.next, before := (s.next, s.slot) :: s.before }
  -- decide_permission, telegram.py:2844-2856: only when nothing is awaited; saves nothing.
  | .perm => if s.awaiting then s else
      { s with next := s.next + 1, live := s.live ++ [s.next], waiting := s.waiting ++ [s.next],
               slot := some s.next, perms := s.next :: s.perms }
  -- A future settles (telegram.py:1964 typed, 2146 tap, 1813 defer, wait_for's timeout or cancel).
  -- The slot is not touched.
  | .settle i => { s with live := s.live.filter (· ≠ i) }
  -- The `finally` of question `i`, after its future settled.
  | .finish i =>
      if s.waiting.contains i && !s.live.contains i then
        let s' := { s with waiting := s.waiting.filter (· ≠ i) }
        if s.slot = some i then
          if s.perms.contains i then
            -- decide_permission, telegram.py:2875-2878: the slot is emptied.
            { s' with slot := none }
          else
            -- _ask_user, telegram.py:3432-3440: `before` comes back only if it is still not done.
            match s.before.lookup i with
            | some (some b) => if s.live.contains b then { s' with slot := some b }
                               else { s' with slot := none }
            | _ => { s' with slot := none }
        else s'                                     -- telegram.py:3432/2875: not the slot, nothing
      else s

def Cur.run (evs : List Event) : Cur := evs.foldl Cur.step {}

/-- Three nested questions: A (0), B (1), C (2). B goes away, then C is answered. A is still live,
yet the slot is empty: A's typed answer no longer reaches it. -/
theorem current_violates :
    let s := Cur.run [.ask, .ask, .ask, .settle 1, .finish 1, .settle 2, .finish 2]
    s.live = [0] ∧ s.slot = none ∧ ¬ Spec s.live s.slot := by
  decide

/-! ## The fixed code

  `self._questions` is a list of `_Question` entries in the order they opened (telegram.py, fixed).
  Every open appends one, every `finally` removes its own, and `_show_question` sets the slot to the
  last entry whose future is not done. A done-callback on each future runs `_show_question` when it
  settles; it is modelled as part of `settle`. `live` is ghost state updated exactly as in `Cur`, so
  the property is stated about the same thing in both models. -/

structure Fix where
  next : Nat := 0
  live : List Nat := []                     -- ghost: opened, future not settled
  stack : List (Nat × Bool) := []           -- `_questions`: (question, future done)
  slot : Option Nat := none                 -- `_pending_answer`
  deriving Repr

/-- `_show_question` (telegram.py:3409-3418, fixed): the last entry whose future is not done. -/
def top : List (Nat × Bool) → Option Nat
  | [] => none
  | (i, d) :: rest => match top rest with
      | some t => some t
      | none => if d then none else some i

def liveIds : List (Nat × Bool) → List Nat
  | [] => []
  | (i, d) :: rest => if d then liveIds rest else i :: liveIds rest

def markDone (i : Nat) (l : List (Nat × Bool)) : List (Nat × Bool) :=
  l.map (fun e => if e.1 = i then (e.1, true) else e)

def dropDone (i : Nat) : List (Nat × Bool) → List (Nat × Bool)
  | [] => []
  | (j, d) :: rest => if j = i ∧ d = true then dropDone i rest else (j, d) :: dropDone i rest

def Fix.awaiting (s : Fix) : Bool :=
  match s.slot with
  | some j => s.live.contains j
  | none => false

def Fix.step (s : Fix) : Event → Fix
  -- _ask_user, telegram.py:3441 (fixed) -> _open_question 3393-3402: append, then `_show_question`.
  | .ask => let st := s.stack ++ [(s.next, false)]
      { s with next := s.next + 1, live := s.live ++ [s.next], stack := st, slot := top st }
  -- decide_permission, telegram.py:2863 guard, 2880 (fixed): only when nothing is awaited; then the
  -- same `_open_question`.
  | .perm => if s.awaiting then s else
      let st := s.stack ++ [(s.next, false)]
      { s with next := s.next + 1, live := s.live ++ [s.next], stack := st, slot := top st }
  -- A future settles (1983 typed, 2164 tap, 1832 defer, timeout, cancel, fixed lines); the done-callback
  -- registered at telegram.py:3400 runs `_show_question`.
  | .settle i => let st := markDone i s.stack
      { s with live := s.live.filter (· ≠ i), stack := st, slot := top st }
  -- The `finally` (telegram.py:3476 and 2895, fixed) -> _close_question 3404-3407: the asker removes
  -- its own (done) entry, then `_show_question`.
  | .finish i => let st := dropDone i s.stack
      { s with stack := st, slot := top st }

def Fix.run (evs : List Event) : Fix := evs.foldl Fix.step {}

/-! ### Proof -/

def Ordered (l : List (Nat × Bool)) : Prop := l.Pairwise (fun a b => a.1 < b.1)

/-- What every reachable fixed state satisfies. -/
structure Inv (s : Fix) : Prop where
  ordered : Ordered s.stack
  bound : ∀ e ∈ s.stack, e.1 < s.next
  ghost : liveIds s.stack = s.live
  slot : s.slot = top s.stack

theorem top_mem : ∀ (l : List (Nat × Bool)) (t : Nat), top l = some t → ∃ d, (t, d) ∈ l
  | [], t, h => by simp [top] at h
  | (i, d) :: rest, t, h => by
      simp only [top] at h
      cases hr : top rest with
      | some u =>
          rw [hr] at h
          obtain ⟨d', hd'⟩ := top_mem rest u hr
          simp at h; subst h
          exact ⟨d', List.mem_cons_of_mem _ hd'⟩
      | none =>
          rw [hr] at h
          cases d with
          | true => simp at h
          | false => simp at h; subst h; exact ⟨false, List.mem_cons_self⟩

theorem top_eq_max : ∀ l : List (Nat × Bool), Ordered l → top l = maxOf (liveIds l)
  | [], _ => rfl
  | (i, d) :: rest, h => by
      have hp := List.pairwise_cons.mp h
      have ih := top_eq_max rest hp.2
      cases hr : top rest with
      | some t =>
          obtain ⟨d', hmem⟩ := top_mem rest t hr
          have hlt : i < t := hp.1 (t, d') hmem
          rw [hr] at ih
          cases d with
          | true => simp [top, liveIds, hr, ← ih]
          | false =>
              simp only [top, liveIds, hr, Bool.false_eq_true, ↓reduceIte, maxOf, ← ih]
              simp [Nat.le_of_lt hlt]
      | none =>
          rw [hr] at ih
          cases d with
          | true => simp [top, liveIds, hr, ← ih]
          | false => simp [top, liveIds, hr, maxOf, ← ih]

theorem liveIds_append (l : List (Nat × Bool)) (n : Nat) :
    liveIds (l ++ [(n, false)]) = liveIds l ++ [n] := by
  induction l with
  | nil => simp [liveIds]
  | cons e rest ih =>
      obtain ⟨i, d⟩ := e
      cases d <;> simp [liveIds, ih]

theorem liveIds_markDone (i : Nat) (l : List (Nat × Bool)) :
    liveIds (markDone i l) = (liveIds l).filter (· ≠ i) := by
  induction l with
  | nil => simp [liveIds, markDone]
  | cons e rest ih =>
      obtain ⟨j, d⟩ := e
      simp only [markDone, List.map_cons] at ih ⊢
      by_cases hj : j = i
      · subst hj; cases d <;> simp [liveIds, ih]
      · cases d <;> simp [liveIds, hj, ih]

theorem liveIds_dropDone (i : Nat) (l : List (Nat × Bool)) :
    liveIds (dropDone i l) = liveIds l := by
  induction l with
  | nil => simp [liveIds, dropDone]
  | cons e rest ih =>
      obtain ⟨j, d⟩ := e
      by_cases hj : j = i <;> cases d <;> simp [dropDone, liveIds, hj, ih]

theorem dropDone_sublist (i : Nat) (l : List (Nat × Bool)) : (dropDone i l).Sublist l := by
  induction l with
  | nil => simp [dropDone]
  | cons e rest ih =>
      obtain ⟨j, d⟩ := e
      by_cases h : j = i ∧ d = true
      · simp only [dropDone, h, and_self, ↓reduceIte]; exact ih.cons _
      · simp only [dropDone, h, ↓reduceIte]; exact ih.cons_cons _

theorem inv_push (s : Fix) (h : Inv s) :
    Ordered (s.stack ++ [(s.next, false)]) ∧
      ∀ e ∈ s.stack ++ [(s.next, false)], e.1 < s.next + 1 := by
  refine ⟨?_, ?_⟩
  · unfold Ordered
    rw [List.pairwise_append]
    refine ⟨h.ordered, by simp, ?_⟩
    intro a ha b hb
    simp at hb; subst hb
    exact h.bound a ha
  · intro e he
    rcases List.mem_append.mp he with he | he
    · exact Nat.lt_succ_of_lt (h.bound e he)
    · simp at he; subst he; exact Nat.lt_succ_self _

theorem inv_step (s : Fix) (e : Event) (h : Inv s) : Inv (s.step e) := by
  cases e with
  | ask =>
      obtain ⟨ho, hb⟩ := inv_push s h
      exact ⟨ho, hb, by simp [Fix.step, liveIds_append, h.ghost], rfl⟩
  | perm =>
      by_cases ha : s.awaiting
      · simp only [Fix.step, ha, ↓reduceIte]; exact h
      · obtain ⟨ho, hb⟩ := inv_push s h
        simp only [Fix.step, ha, Bool.false_eq_true, ↓reduceIte]
        exact ⟨ho, hb, by simp [liveIds_append, h.ghost], rfl⟩
  | settle i =>
      refine ⟨?_, ?_, ?_, rfl⟩
      · show Ordered (markDone i s.stack)
        unfold Ordered markDone
        rw [List.pairwise_map]
        refine List.Pairwise.imp ?_ h.ordered
        intro a b hab
        by_cases ha : a.1 = i <;> by_cases hb : b.1 = i <;> simp [ha, hb] <;> omega
      · intro x hx
        simp only [Fix.step, markDone, List.mem_map] at hx
        obtain ⟨y, hy, rfl⟩ := hx
        by_cases hi : y.1 = i <;> simp [hi] <;> first | exact h.bound y hy | (rw [← hi]; exact h.bound y hy)
      · simp [Fix.step, liveIds_markDone, h.ghost]
  | finish i =>
      refine ⟨?_, ?_, ?_, rfl⟩
      · show Ordered (dropDone i s.stack)
        exact h.ordered.sublist (dropDone_sublist i s.stack)
      · intro x hx
        exact h.bound x ((dropDone_sublist i s.stack).subset hx)
      · simp [Fix.step, liveIds_dropDone, h.ghost]

theorem inv_run (evs : List Event) : ∀ s : Fix, Inv s → Inv (evs.foldl Fix.step s) := by
  induction evs with
  | nil => intro s h; exact h
  | cons e rest ih => intro s h; exact ih _ (inv_step s e h)

/-- For every trace, the slot names the most recently opened live question (and is empty when none
is live): a typed answer always reaches the innermost live question. -/
theorem fixed_invariant (evs : List Event) :
    (Fix.run evs).slot = maxOf (Fix.run evs).live ∧ Spec (Fix.run evs).live (Fix.run evs).slot := by
  have h := inv_run evs {} ⟨List.Pairwise.nil, by simp, rfl, rfl⟩
  have hs : (Fix.run evs).slot = maxOf (Fix.run evs).live := by
    unfold Fix.run
    rw [h.slot, top_eq_max _ h.ordered, h.ghost]
  exact ⟨hs, fun _ => hs⟩

/-- The same trace that breaks the current code keeps A in the slot once fixed. -/
theorem fixed_replays_counterexample :
    (Fix.run [.ask, .ask, .ask, .settle 1, .finish 1, .settle 2, .finish 2]).slot = some 0 := by
  decide

end Eve.Questions
