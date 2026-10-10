/-
  The watcher's scheduler (bridge/src/eve_bridge/watch.py, class Watcher): its loop (`run`, `tick`,
  `_run_one`, `check`) and its chat tools (`add`, `remove`), on one asyncio event loop. Everything between
  two awaits is atomic. The Telegram brain calls one tool at a time (telegram.py sends
  `parallel_tool_calls: False`, and a turn runs under `_turn_lock`), but the loop runs beside a turn.

  Line numbers are those of watch.py as verified (1792 lines, before the fix).

  Part 1, the list. The properties:

    (1) no two watches on the list share an id, and no id the owner was ever given names a second watch;
    (2) a watch the owner removed is never checked again once `remove()` returns, and nothing more is
        texted about it, even when `tick` had already picked it or its check was already reading.

  The current code keeps the list's ids distinct and never starts a check of a removed watch (the guard
  `if w not in self.watches`, :1476, runs after the lock with no await before the read). It breaks the
  rest: a check already reading when the watch is removed still texts its alert (:1487-1489), its
  "I can't read" (:1482-1484) and its "I can read X again" (:1453-1455); and `_new_id` (:1573-1578) is
  1 + the largest id on the list, so removing the newest watch hands its id to the next one.

  Abstraction. A watch is a number (`o`, the Python object, fresh per `validate`) with an id (`wN` as
  `N`). `pick o` is tick taking `o` from its sorted snapshot (:1495); any watch ever made may be picked,
  which covers a snapshot that is stale by any amount. `enter` is `_run_one` getting the lock and running
  the guard; `ret r` is the check's read returning (:1434) with what it leads to; each text is its own
  `send`, since each `_tell` is an await. `validate` is `add` calling `validate` -> `_new_id` (:1633);
  `queue` is the limiter-busy path (:1643-1646), `lock` the `async with self._lock` of :1650, `keep` the
  append of :1665, `drop` a 400/404/410 or unknown symbol (:1660-1663). `remove i` is `remove()`
  (:1564-1571): the first watch with id `i` leaves the list. The Watcher's lock is left out: letting `add`'s
  first check and the loop's check overlap only adds traces, so an invariant proved without it holds with
  it. `add` is one at a time (the turn lock); `remove` may come at any point, even inside an `add`, which
  again only adds traces. `w not in self.watches` compares dataclasses by value; it is modelled as identity,
  which it equals whenever two watches differ in any field (their ids, by (1)). Proofs are over every trace,
  by induction, with no bound.

  Part 2, the via ladder and the error notices, per watch (`Ladder`, `Notice`); part 3, the loop's wake-up
  (`Wake`); part 4, a check that raises something other than FetchError (`Starve`). Each says what it
  abstracts where it starts.

  After this model (2026-09-25): a send the owner never received is outside it. `_tell` now returns whether the
  notice went; an alert Telegram refused is kept on the watch (`pending`) and sent again at the next tick, and
  `told_error` is set only when "I can't read" went. With every send succeeding, as here, both are the modelled
  behaviour.
-/
namespace Eve.WatchScheduler

/-! ## Part 1: the list, `add`, `remove` and the loop -/

/-- What one check came to: the texts it goes on to send, in order. -/
inductive Result where
  | quiet        -- no alert and no notice (also a 401/403 that moved the ladder: check returns None, :1443)
  | alert        -- evaluate returned an alert: _run_one texts it after the lock (:1487-1489)
  | again        -- a told streak ended: "I can read X again" (:1453-1455), no alert
  | againAlert   -- both: the notice inside check, then the alert
  | cantRead     -- a FetchError, the 4th in a row, not yet told: "I can't read X" (:1482-1484)
  deriving DecidableEq, Repr

def Result.texts : Result → Nat
  | .quiet => 0
  | .alert => 1
  | .again => 1
  | .againAlert => 2
  | .cantRead => 1

inductive Loop where
  | idle                          -- asleep, or tick between two watches
  | picked (o : Nat)              -- tick took o (:1495); _run_one awaits the lock (:1475)
  | reading (o : Nat)             -- the guard passed (:1476); check's read is in flight (:1434)
  | sending (o : Nat) (n : Nat)   -- the check is over; n texts still to go, each an await
  deriving DecidableEq, Repr

inductive Adding where
  | none
  | waiting (o i : Nat)           -- validate made watch o with id i; add sleeps on the limiter (:1638-1640)
  | checking (o i : Nat)          -- add holds the lock; its first check(s) are in flight (:1650-1658)
  deriving DecidableEq, Repr

def Adding.slot : Adding → Option (Nat × Nat)
  | .none => Option.none
  | .waiting o i => Option.some (o, i)
  | .checking o i => Option.some (o, i)

inductive Event where
  | pick (o : Nat)
  | enter
  | ret (r : Result)
  | send
  | validate
  | queue
  | lock
  | keep
  | drop
  | remove (i : Nat)
  deriving DecidableEq, Repr

structure S where
  nextObj : Nat := 0
  list : List (Nat × Nat) := []    -- self.watches: (watch, its id), in order
  adding : Adding := .none
  loop : Loop := .idle
  hw : Nat := 0                    -- _last_id (the fix); the current code has none
  removed : List Nat := []         -- ghost: the watches whose remove() returned
  named : List Nat := []           -- ghost: the ids handed to the owner (add's result, :1641)
  checkedRemoved : Bool := false   -- ghost: a check started on a removed watch
  textedRemoved : Bool := false    -- ghost: a text went out about a removed watch
  deriving DecidableEq, Repr

def objs (l : List (Nat × Nat)) : List Nat := l.map Prod.fst
def ids (l : List (Nat × Nat)) : List Nat := l.map Prod.snd

/-- `w in self.watches`. -/
def has (l : List (Nat × Nat)) (o : Nat) : Bool := (objs l).contains o

def maxOf : List Nat → Nat
  | [] => 0
  | a :: l => max a (maxOf l)

def nodupB : List Nat → Bool
  | [] => true
  | a :: l => !l.contains a && nodupB l

/-- `remove()` (:1564-1571): the first watch whose id is `i` leaves the list. -/
def dropId (i : Nat) : List (Nat × Nat) → Option Nat × List (Nat × Nat)
  | [] => (none, [])
  | (o, j) :: rest => if j = i then (some o, rest) else ((dropId i rest).1, (o, j) :: (dropId i rest).2)

/-- `_new_id`. Current (:1573-1578): 1 + the largest id on the list (the `while` never runs: that number is
free). Fixed: 1 + the larger of that and `_last_id`, which it then becomes. -/
def newId (fixed : Bool) (s : S) : Nat :=
  if fixed then max s.hw (maxOf (ids s.list)) + 1 else maxOf (ids s.list) + 1

/-- The property: ids distinct on the list and never handed out twice; no check of a removed watch; no text
about one. -/
def Spec (s : S) : Bool :=
  nodupB (ids s.list) && !s.checkedRemoved && !s.textedRemoved && nodupB s.named

/-- One step; `fixed` picks the current code (`false`) or the fixed code (`true`). -/
def step (fixed : Bool) (s : S) : Event → S
  | .pick o => match s.loop with
      | .idle => if o < s.nextObj then { s with loop := .picked o } else s
      | _ => s
  -- :1475-1477, after the lock: `if w not in self.watches: return`, then check's read starts with no await
  -- before it (the fix keeps the guard, by identity: `_kept`).
  | .enter => match s.loop with
      | .picked o => if has s.list o then
            { s with loop := .reading o, checkedRemoved := s.checkedRemoved || s.removed.contains o }
          else { s with loop := .idle }
      | _ => s
  | .ret r => match s.loop with
      | .reading o => { s with loop := if r.texts = 0 then .idle else .sending o r.texts }
      | _ => s
  -- a `_tell`. Current (:1453, :1482, :1487): sent whatever the list says. Fixed: only `if self._kept(w)`,
  -- checked right before the `_tell` with no await between.
  | .send => match s.loop with
      | .sending o n =>
          let next := if n ≤ 1 then Loop.idle else Loop.sending o (n - 1)
          if fixed && !has s.list o then { s with loop := next }
          else { s with loop := next, textedRemoved := s.textedRemoved || s.removed.contains o }
      | _ => s
  -- :1633 -> :1626 `Watch(id=self._new_id(), ...)`; one add at a time (the turn lock).
  | .validate => match s.adding with
      | .none => { s with adding := .waiting s.nextObj (newId fixed s), nextObj := s.nextObj + 1,
                          hw := if fixed then newId fixed s else s.hw }
      | _ => s
  -- :1643-1647: the limiter is still busy; append, save, wake, and the id goes back to the owner.
  | .queue => match s.adding with
      | .waiting o i => { s with list := s.list ++ [(o, i)], named := s.named ++ [i], adding := .none }
      | _ => s
  | .lock => match s.adding with
      | .waiting o i => { s with adding := .checking o i }
      | _ => s
  -- :1665-1667 and the result's id (:1641).
  | .keep => match s.adding with
      | .checking o i => { s with list := s.list ++ [(o, i)], named := s.named ++ [i], adding := .none }
      | _ => s
  | .drop => match s.adding with
      | .checking _ _ => { s with adding := .none }
      | _ => s
  | .remove i => match dropId i s.list with
      | (some o, rest) => { s with list := rest, removed := o :: s.removed }
      | (none, _) => s

def Cur.run (evs : List Event) : S := evs.foldl (step false) {}
def Fix.run (evs : List Event) : S := evs.foldl (step true) {}

/-- The owner adds a watch (w1); the loop picks it and its read starts; the owner removes w1 and
`remove()` returns; the read comes back past the mark, and the alert is texted about a watch the owner
has just stopped. -/
theorem current_violates :
    let s := Cur.run [.validate, .queue, .pick 0, .enter, .remove 1, .ret .alert, .send]
    s.list = [] ∧ s.removed = [0] ∧ s.textedRemoved = true ∧ Spec s = false := by
  decide

/-- The same with the fourth failure in a row: "I can't read" goes out about the removed watch. -/
theorem current_violates_cant_read :
    (Cur.run [.validate, .queue, .pick 0, .enter, .remove 1, .ret .cantRead, .send]).textedRemoved = true := by
  decide

/-- w1 is added, removed, and the next watch is w1 again: the owner's listing, and the transcript's
watch_add result, now name a different watch by the same id. -/
theorem current_reuses_id :
    let s := Cur.run [.validate, .queue, .remove 1, .validate, .queue]
    s.named = [1, 1] ∧ s.list = [(1, 1)] ∧ Spec s = false := by
  decide

/-! ### The invariant, for both the current and the fixed step -/

def SlotOk (s : S) : Prop :=
  ∀ o i, s.adding.slot = some (o, i) →
    (∀ x ∈ s.list, x.1 < o ∧ x.2 < i) ∧ (∀ r ∈ s.removed, r < o) ∧ o < s.nextObj

structure Inv (s : S) : Prop where
  ids_sorted : s.list.Pairwise (fun a b => a.2 < b.2)
  objs_sorted : s.list.Pairwise (fun a b => a.1 < b.1)
  obj_lt : ∀ x ∈ s.list, x.1 < s.nextObj
  rem_lt : ∀ r ∈ s.removed, r < s.nextObj
  disjoint : ∀ x ∈ s.list, x.1 ∉ s.removed
  slot : SlotOk s
  checked : s.checkedRemoved = false

theorem has_iff (l : List (Nat × Nat)) (o : Nat) : has l o = true ↔ ∃ x ∈ l, x.1 = o := by
  unfold has objs
  rw [List.contains_iff_mem, List.mem_map]

theorem le_maxOf : ∀ (l : List Nat) (a : Nat), a ∈ l → a ≤ maxOf l
  | [], _, h => by simp at h
  | b :: l, a, h => by
      simp only [maxOf]
      rcases List.mem_cons.mp h with h | h
      · subst h; exact Nat.le_max_left _ _
      · exact Nat.le_trans (le_maxOf l a h) (Nat.le_max_right _ _)

theorem id_lt_newId (fixed : Bool) (s : S) : ∀ x ∈ s.list, x.2 < newId fixed s := by
  intro x hx
  have hm : x.2 ≤ maxOf (ids s.list) := le_maxOf _ _ (List.mem_map.mpr ⟨x, hx, rfl⟩)
  unfold newId
  cases fixed
  · simp only [Bool.false_eq_true, ↓reduceIte]; omega
  · simp only [↓reduceIte]
    have := Nat.le_max_right s.hw (maxOf (ids s.list))
    omega

theorem nodupB_of_sorted : ∀ l : List Nat, l.Pairwise (· < ·) → nodupB l = true
  | [], _ => rfl
  | a :: l, h => by
      have hp := List.pairwise_cons.mp h
      simp only [nodupB, Bool.and_eq_true, Bool.not_eq_true']
      refine ⟨?_, nodupB_of_sorted l hp.2⟩
      cases hc : l.contains a with
      | false => rfl
      | true =>
          have := hp.1 a (List.contains_iff_mem.mp hc)
          omega

theorem dropId_some : ∀ (i : Nat) (l : List (Nat × Nat)) (o : Nat) (rest : List (Nat × Nat)),
    dropId i l = (some o, rest) → ∃ pre post, l = pre ++ (o, i) :: post ∧ rest = pre ++ post
  | _, [], _, _, h => by simp [dropId] at h
  | i, (p, j) :: l, o, rest, h => by
      simp only [dropId] at h
      by_cases hj : j = i
      · simp only [hj, ↓reduceIte, Prod.mk.injEq, Option.some.injEq] at h
        obtain ⟨rfl, rfl⟩ := h
        exact ⟨[], l, by simp [hj], rfl⟩
      · simp only [hj, ↓reduceIte, Prod.mk.injEq] at h
        obtain ⟨h1, h2⟩ := h
        obtain ⟨pre, post, hl, hr⟩ := dropId_some i l o (dropId i l).2 (by rw [← h1])
        exact ⟨(p, j) :: pre, post, by simp [hl], by rw [← h2, hr]; rfl⟩

theorem inv_append (s : S) (o i : Nat) (h : Inv s) (hs : s.adding.slot = some (o, i)) :
    (s.list ++ [(o, i)]).Pairwise (fun a b => a.2 < b.2) ∧
    (s.list ++ [(o, i)]).Pairwise (fun a b => a.1 < b.1) ∧
    (∀ x ∈ s.list ++ [(o, i)], x.1 < s.nextObj) ∧
    (∀ x ∈ s.list ++ [(o, i)], x.1 ∉ s.removed) := by
  obtain ⟨hlt, hrem, hon⟩ := h.slot o i hs
  refine ⟨?_, ?_, ?_, ?_⟩
  · rw [List.pairwise_append]
    refine ⟨h.ids_sorted, List.pairwise_singleton _ _, ?_⟩
    intro a ha b hb; simp at hb; subst hb; exact (hlt a ha).2
  · rw [List.pairwise_append]
    refine ⟨h.objs_sorted, List.pairwise_singleton _ _, ?_⟩
    intro a ha b hb; simp at hb; subst hb; exact (hlt a ha).1
  · intro x hx
    rcases List.mem_append.mp hx with hx | hx
    · exact h.obj_lt x hx
    · simp at hx; subst hx; exact hon
  · intro x hx
    rcases List.mem_append.mp hx with hx | hx
    · exact h.disjoint x hx
    · simp at hx; subst hx; intro hr; have := hrem _ hr; simp at this

/-- Only the fields the invariant reads matter. -/
theorem inv_of_eq (s t : S) (h : Inv s) (h1 : t.list = s.list) (h2 : t.removed = s.removed)
    (h3 : t.adding.slot = s.adding.slot) (h4 : t.nextObj = s.nextObj)
    (h5 : t.checkedRemoved = s.checkedRemoved) : Inv t :=
  ⟨by rw [h1]; exact h.ids_sorted, by rw [h1]; exact h.objs_sorted, by rw [h1, h4]; exact h.obj_lt,
   by rw [h2, h4]; exact h.rem_lt, by rw [h1, h2]; exact h.disjoint,
   by unfold SlotOk; rw [h1, h2, h3, h4]; exact h.slot, by rw [h5]; exact h.checked⟩

theorem not_removed_of_has (s : S) (h : Inv s) (o : Nat) (hh : has s.list o = true) :
    s.removed.contains o = false := by
  obtain ⟨x, hx, rfl⟩ := (has_iff _ _).mp hh
  cases hc : s.removed.contains x.1 with
  | false => rfl
  | true => exact absurd (List.contains_iff_mem.mp hc) (h.disjoint x hx)

theorem inv_step (fixed : Bool) (s : S) (e : Event) (h : Inv s) : Inv (step fixed s e) := by
  rcases s with ⟨no, l, ad, lp, hw, rm, nm, cr, tr⟩
  cases e with
  | pick o =>
      cases lp <;> simp only [step] <;> (try split) <;> exact inv_of_eq _ _ h rfl rfl rfl rfl rfl
  | enter =>
      cases lp with
      | picked o =>
          simp only [step]; split
          · rename_i hh
            have hc := not_removed_of_has _ h o hh
            exact inv_of_eq _ _ h rfl rfl rfl rfl (by simp only at hc ⊢; rw [hc, Bool.or_false])
          · exact inv_of_eq _ _ h rfl rfl rfl rfl rfl
      | _ => simp only [step]; exact h
  | ret r =>
      cases lp <;> simp only [step] <;> exact inv_of_eq _ _ h rfl rfl rfl rfl rfl
  | send =>
      cases lp <;> simp only [step] <;> (try split) <;> exact inv_of_eq _ _ h rfl rfl rfl rfl rfl
  | validate =>
      cases ad with
      | none =>
          simp only [step]
          refine ⟨h.ids_sorted, h.objs_sorted, fun x hx => Nat.lt_succ_of_lt (h.obj_lt x hx),
            fun r hr => Nat.lt_succ_of_lt (h.rem_lt r hr), h.disjoint, ?_, h.checked⟩
          intro o i hs
          simp only [Adding.slot, Option.some.injEq, Prod.mk.injEq] at hs
          obtain ⟨rfl, rfl⟩ := hs
          exact ⟨fun x hx => ⟨h.obj_lt x hx, id_lt_newId fixed _ x hx⟩, h.rem_lt, Nat.lt_succ_self _⟩
      | _ => simp only [step]; exact h
  | queue =>
      cases ad with
      | waiting o i =>
          simp only [step]
          obtain ⟨a, b, c, d⟩ := inv_append _ o i h rfl
          exact ⟨a, b, c, h.rem_lt, d, fun _ _ hs' => by simp [Adding.slot] at hs', h.checked⟩
      | _ => simp only [step]; exact h
  | lock =>
      cases ad with
      | waiting o i => simp only [step]; exact inv_of_eq _ _ h rfl rfl rfl rfl rfl
      | _ => simp only [step]; exact h
  | keep =>
      cases ad with
      | checking o i =>
          simp only [step]
          obtain ⟨a, b, c, d⟩ := inv_append _ o i h rfl
          exact ⟨a, b, c, h.rem_lt, d, fun _ _ hs' => by simp [Adding.slot] at hs', h.checked⟩
      | _ => simp only [step]; exact h
  | drop =>
      cases ad with
      | checking o i =>
          simp only [step]
          exact ⟨h.ids_sorted, h.objs_sorted, h.obj_lt, h.rem_lt, h.disjoint,
            fun _ _ hs' => by simp [Adding.slot] at hs', h.checked⟩
      | _ => simp only [step]; exact h
  | remove i =>
      simp only [step]
      split
      · rename_i o rest hd
        obtain ⟨pre, post, hl, hr⟩ := dropId_some i l o rest hd
        have hsub : rest.Sublist l := by
          rw [hl, hr]; exact List.Sublist.append (List.Sublist.refl pre) (List.sublist_cons_self _ _)
        have hmem : (o, i) ∈ l := by rw [hl]; simp
        have hne : ∀ x ∈ rest, x.1 ≠ o := by
          have hp := h.objs_sorted
          simp only at hp
          rw [hl, List.pairwise_append] at hp
          obtain ⟨_, hpost, hcross⟩ := hp
          intro x hx
          rw [hr] at hx
          rcases List.mem_append.mp hx with hx | hx
          · have := hcross x hx (o, i) (by simp); simp at this; omega
          · have := (List.pairwise_cons.mp hpost).1 x hx; simp at this; omega
        refine ⟨h.ids_sorted.sublist hsub, h.objs_sorted.sublist hsub,
          fun x hx => h.obj_lt x (hsub.subset hx), ?_, ?_, ?_, h.checked⟩
        · intro r hr'
          rcases List.mem_cons.mp hr' with hr' | hr'
          · subst hr'; exact h.obj_lt _ hmem
          · exact h.rem_lt r hr'
        · intro x hx hx'
          rcases List.mem_cons.mp hx' with hx' | hx'
          · exact hne x hx hx'
          · exact h.disjoint x (hsub.subset hx) hx'
        · intro so si hs
          obtain ⟨hlt, hrem, hon⟩ := h.slot so si hs
          refine ⟨fun x hx => hlt x (hsub.subset hx), ?_, hon⟩
          intro r hr'
          rcases List.mem_cons.mp hr' with hr' | hr'
          · subst hr'; exact (hlt _ hmem).1
          · exact hrem r hr'
      · exact h

theorem inv_run (fixed : Bool) (evs : List Event) : ∀ s : S, Inv s → Inv (evs.foldl (step fixed) s) := by
  induction evs with
  | nil => intro s h; exact h
  | cons e rest ih => intro s h; exact ih _ (inv_step fixed s e h)

theorem inv_init : Inv ({} : S) :=
  ⟨List.Pairwise.nil, List.Pairwise.nil, by simp, by simp, by simp,
   fun _ _ h => by simp [Adding.slot] at h, rfl⟩

/-- What the current code gets right, over every trace: the ids on the list are distinct (property 1's
first half), and no check of a removed watch ever starts (property 2's first half, the guard). -/
theorem current_ids_unique_and_no_check_after_remove (evs : List Event) :
    nodupB (ids (Cur.run evs).list) = true ∧ (Cur.run evs).checkedRemoved = false := by
  have h := inv_run false evs {} inv_init
  exact ⟨nodupB_of_sorted _ (List.pairwise_map.mpr h.ids_sorted), h.checked⟩

/-! ### The fixed code: the texts and the ids -/

structure FInv (s : S) : Prop where
  base : Inv s
  texted : s.textedRemoved = false
  named_sorted : s.named.Pairwise (· < ·)
  named_le : ∀ n ∈ s.named, n ≤ s.hw
  slot_hw : ∀ o i, s.adding.slot = some (o, i) → i = s.hw ∧ ∀ n ∈ s.named, n < i

theorem finv_of_eq (s t : S) (h : FInv s) (hb : Inv t) (h1 : t.named = s.named) (h2 : t.hw = s.hw)
    (h3 : t.adding.slot = s.adding.slot) (h4 : t.textedRemoved = s.textedRemoved) : FInv t :=
  ⟨hb, by rw [h4]; exact h.texted, by rw [h1]; exact h.named_sorted, by rw [h1, h2]; exact h.named_le,
   by rw [h1, h2, h3]; exact h.slot_hw⟩

theorem finv_step (s : S) (e : Event) (h : FInv s) : FInv (step true s e) := by
  have hb := inv_step true s e h.base
  rcases s with ⟨no, l, ad, lp, hw, rm, nm, cr, tr⟩
  cases e with
  | pick o =>
      cases lp <;> simp only [step] at hb ⊢ <;> (try split at hb) <;> (try split) <;>
        first | exact h | exact finv_of_eq _ _ h hb rfl rfl rfl rfl | skip
      all_goals simp_all
  | enter =>
      cases lp <;> simp only [step] at hb ⊢ <;> (try split at hb) <;> (try split) <;>
        first | exact h | exact finv_of_eq _ _ h hb rfl rfl rfl rfl | skip
      all_goals simp_all
  | ret r =>
      cases lp <;> simp only [step] at hb ⊢ <;> first | exact h | exact finv_of_eq _ _ h hb rfl rfl rfl rfl
  | send =>
      cases lp with
      | sending o n =>
          simp only [step] at hb ⊢
          split
          · rename_i hk
            simp only [hk, ↓reduceIte] at hb
            exact finv_of_eq _ _ h hb rfl rfl rfl rfl
          · rename_i hk
            simp only [hk, ↓reduceIte, Bool.false_eq_true] at hb
            have hh : has l o = true := by simpa using hk
            have hc := not_removed_of_has _ h.base o hh
            simp only at hc
            exact finv_of_eq _ _ h hb rfl rfl rfl (by simp only; rw [hc, Bool.or_false])
      | _ => simp only [step] at hb ⊢; exact h
  | validate =>
      cases ad with
      | none =>
          simp only [step] at hb ⊢
          have hgt : hw < newId true ⟨no, l, .none, lp, hw, rm, nm, cr, tr⟩ := by
            unfold newId; simp only [↓reduceIte]
            have := Nat.le_max_left hw (maxOf (ids l)); omega
          refine ⟨hb, h.texted, h.named_sorted, ?_, ?_⟩
          · intro n hn'; have := h.named_le n hn'; dsimp only at this hn' ⊢; simp only [↓reduceIte]; omega
          · intro o i hs
            simp only [Adding.slot, Option.some.injEq, Prod.mk.injEq] at hs
            obtain ⟨rfl, rfl⟩ := hs
            refine ⟨by simp, fun n hn' => ?_⟩
            have := h.named_le n hn'; dsimp only at this hn' ⊢; omega
      | _ => simp only [step] at hb ⊢; exact h
  | queue =>
      cases ad with
      | waiting o i =>
          simp only [step] at hb ⊢
          obtain ⟨hi, hlt⟩ := h.slot_hw o i rfl
          refine ⟨hb, h.texted, ?_, ?_, ?_⟩
          · rw [List.pairwise_append]
            exact ⟨h.named_sorted, List.pairwise_singleton _ _,
              fun a ha b hb' => by simp at hb'; subst hb'; exact hlt a ha⟩
          · intro n hn'
            rcases List.mem_append.mp hn' with hn' | hn'
            · exact h.named_le n hn'
            · simp at hn' hi; dsimp only; omega
          · intro _ _ hs'; simp [Adding.slot] at hs'
      | _ => simp only [step] at hb ⊢; exact h
  | lock =>
      cases ad with
      | waiting o i => simp only [step] at hb ⊢; exact finv_of_eq _ _ h hb rfl rfl rfl rfl
      | _ => simp only [step] at hb ⊢; exact h
  | keep =>
      cases ad with
      | checking o i =>
          simp only [step] at hb ⊢
          obtain ⟨hi, hlt⟩ := h.slot_hw o i rfl
          refine ⟨hb, h.texted, ?_, ?_, ?_⟩
          · rw [List.pairwise_append]
            exact ⟨h.named_sorted, List.pairwise_singleton _ _,
              fun a ha b hb' => by simp at hb'; subst hb'; exact hlt a ha⟩
          · intro n hn'
            rcases List.mem_append.mp hn' with hn' | hn'
            · exact h.named_le n hn'
            · simp at hn' hi; dsimp only; omega
          · intro _ _ hs'; simp [Adding.slot] at hs'
      | _ => simp only [step] at hb ⊢; exact h
  | drop =>
      cases ad with
      | checking o i =>
          simp only [step] at hb ⊢
          exact ⟨hb, h.texted, h.named_sorted, h.named_le, fun _ _ hs' => by simp [Adding.slot] at hs'⟩
      | _ => simp only [step] at hb ⊢; exact h
  | remove i =>
      rcases hd : dropId i l with ⟨_ | o, rest⟩ <;> simp only [step, hd] at hb ⊢
      · exact h
      · exact finv_of_eq _ _ h hb rfl rfl rfl rfl

theorem finv_run (evs : List Event) : ∀ s : S, FInv s → FInv (evs.foldl (step true) s) := by
  induction evs with
  | nil => intro s h; exact h
  | cons e rest ih => intro s h; exact ih _ (finv_step s e h)

/-- For every trace of the loop, `add` and `remove`, with the fix: the ids on the list are distinct, no id
is ever handed to the owner twice, no check of a removed watch starts, and nothing is texted about one. -/
theorem fixed_invariant (evs : List Event) : Spec (Fix.run evs) = true := by
  have h := finv_run evs {} ⟨inv_init, rfl, List.Pairwise.nil, by simp, fun _ _ h => by simp [Adding.slot] at h⟩
  unfold Spec
  simp only [Bool.and_eq_true, Bool.not_eq_true']
  exact ⟨⟨⟨nodupB_of_sorted _ (List.pairwise_map.mpr h.base.ids_sorted), h.base.checked⟩, h.texted⟩,
    nodupB_of_sorted _ h.named_sorted⟩

/-- The traces that break the current code, on the fixed one: no text, and a fresh id. -/
theorem fixed_replays_counterexamples :
    (Fix.run [.validate, .queue, .pick 0, .enter, .remove 1, .ret .alert, .send]).textedRemoved = false ∧
    (Fix.run [.validate, .queue, .remove 1, .validate, .queue]).named = [1, 2] := by
  decide

/-! ## Part 2: the via ladder and the error notices, per watch

  Every check of a watch on the list runs under the Watcher's lock (`_run_one`, :1475), and `add`'s first
  checks run under it too before the watch is listed (:1650), so one watch's checks are a sequence. The
  ladder and the notices are two small machines over that sequence. They share only the `refused` event:
  a 401/403 either moves the ladder (:1436-1444, no error counted) or is an ordinary failure, so the notice
  machine takes `refusedLadder` as a step that changes nothing and `failed` for the rest. Each machine
  allows its events in any order, which covers every sequence the combined code can produce.

  `page` is `w.kind == "page"`; `browser` is `self.config.browser` and `tls` is `self.config.tls` and `fc` is
  `_crawl_on()` (all fixed for the process; the Chrome-like read and the hosted rung were added 2026-09-26; the hosted reader is TinyFish). The notice
  machine assumes `self.notify` is set, as the daemon sets it before the loop starts (daemon.py:394); a
  "tell" is the `_tell` call, whether or not Telegram then delivers it. -/

namespace Ladder

inductive Via where
  | plain      -- ""
  | tls        -- "tls": read as Chrome would (curl_cffi); `_fetch_page` goes straight there
  | browser    -- "browser": rendered, then seen (:1286-1287 goes straight there)
  | hosted     -- "hosted": read() goes to _read_crawled, the hosted reader (TinyFish); never rendered
  | search     -- "search": read(): `w.via == "search"` goes to _read_search
  deriving DecidableEq, Repr

def Via.rank : Via → Nat
  | .plain => 0
  | .tls => 1
  | .browser => 2
  | .hosted => 3
  | .search => 4

/-- A ghost count, saturated: `c5` is five or more. -/
inductive Cnt where
  | c0 | c1 | c2 | c3 | c4 | c5
  deriving DecidableEq, Repr

def Cnt.succ : Cnt → Cnt
  | .c0 => .c1
  | .c1 => .c2
  | .c2 => .c3
  | .c3 => .c4
  | .c4 => .c5
  | .c5 => .c5

def Cnt.val : Cnt → Nat
  | .c0 => 0
  | .c1 => 1
  | .c2 => 2
  | .c3 => 3
  | .c4 => 4
  | .c5 => 5

inductive Ev where
  | ok           -- a reading, via unchanged
  | okTls        -- _fetch_page: the plain read was refused (401/403) or never answered and the Chrome-like read
                 -- answered: "" becomes "tls" (whatever the rest of the read then finds)
  | okRendered   -- _read_page's rendered read answered (:1296-1301): "" becomes "browser"
  | crawlOff     -- read(): a "hosted" watch while TinyFish is off (the key removed) goes to search
  | refused      -- a FetchError 401/403 (the site, or the vision model's bot check, :1352-1354)
  deriving DecidableEq, Repr

structure St where
  via : Via := .plain
  moves : Cnt := .c0     -- ghost: how many times via changed
  back : Bool := false   -- ghost: a change that did not go up the ladder
  deriving DecidableEq, Repr

def moveTo (s : St) (v : Via) : St :=
  { via := v, moves := s.moves.succ, back := s.back || !decide (s.via.rank < v.rank) }

def step (page browser tls fc : Bool) (s : St) : Ev → St
  | .ok => s
  -- `_fetch_page`: only with `self.config.tls`, only from ""; a "tls" watch reads that way already.
  | .okTls => if page && tls && s.via == .plain then moveTo s .tls else s
  -- :1296 `if self.config.browser:` and :1286 (a "browser" watch never reaches this path).
  | .okRendered => if page && browser && (s.via == .plain || s.via == .tls) then moveTo s .browser else s
  -- :1436 `e.status in (401, 403) and w.kind == "page" and w.via != "search"`;
  -- `nxt = "browser" if self.config.browser and w.via in ("", "tls") else "hosted" if self._crawl_on() and
  -- w.via in ("", "tls", "browser") else "search"`. Otherwise an error (TinyFish's own refusals carry `host`,
  -- so they are errors, never moves).
  | .crawlOff => if !fc && s.via == .hosted then moveTo s .search else s
  | .refused => if page && s.via != .search then
        moveTo s (if browser && (s.via == .plain || s.via == .tls) then .browser
                  else if fc && s.via != .hosted then .hosted else .search)
      else s

/-- The property: via only goes up, so it changes at most four times. -/
def Spec (s : St) : Bool := !s.back && s.moves != .c5

def Inv (s : St) : Bool := Spec s && decide (s.moves.val ≤ s.via.rank)

end Ladder

open Ladder in
theorem ladder_inv_step (page browser tls fc : Bool) (s : St) (e : Ev) :
    Ladder.Inv s = true → Ladder.Inv (Ladder.step page browser tls fc s e) = true := by
  rcases s with ⟨v, m, b⟩
  cases v <;> cases m <;> cases b <;> cases e <;> cases page <;> cases browser <;> cases tls <;> cases fc <;> decide

open Ladder in
def Ladder.run (page browser tls fc : Bool) (evs : List Ev) : St := evs.foldl (step page browser tls fc) {}

open Ladder in
theorem ladder_inv_run (page browser tls fc : Bool) (evs : List Ev) :
    ∀ s : St, Ladder.Inv s = true → Ladder.Inv (evs.foldl (Ladder.step page browser tls fc) s) = true := by
  induction evs with
  | nil => intro s h; exact h
  | cons e rest ih => intro s h; exact ih _ (ladder_inv_step page browser tls fc s e h)

/-- Property 3, for the current code, over every sequence of checks: the via ladder only goes up ("" ->
"tls" -> "browser" -> "hosted" -> "search", with any rung skipped), so it moves at most four times. The
move to "tls" happens inside a read and never makes a check due again; a refusal's move is the only way a check
is due again at once (:1443) without an error's backoff, so a refusing site gets at most three such immediate
re-checks (`add` allows exactly three); each check is one `read`, which by the code makes at most five requests
("": the fetch, the Chrome-like fetch, the page-text model, the render and the vision model, :1288-1301; "tls":
one fewer; "browser": the render and the vision model; "hosted": one TinyFish fetch and the page-text model;
"search": one model call, :1380-1392). -/
theorem ladder_code_invariant (page browser tls fc : Bool) (evs : List Ladder.Ev) :
    Ladder.Spec (Ladder.run page browser tls fc evs) = true := by
  have h := ladder_inv_run page browser tls fc evs {} (by decide)
  unfold Ladder.Inv at h
  simp only [Bool.and_eq_true] at h
  exact h.1

namespace Notice

/-- `w.errors`, saturated at TELL_AFTER_ERRORS = 4: the code asks only whether it is 0 (:1453) and whether
it is at least 4 (:1482). -/
inductive Errs where
  | e0 | e1 | e2 | e3 | e4
  deriving DecidableEq, Repr

def Errs.succ : Errs → Errs
  | .e0 => .e1
  | .e1 => .e2
  | .e2 => .e3
  | .e3 => .e4
  | .e4 => .e4

inductive Ev where
  | ok              -- the read returned (:1452-1456)
  | failed          -- a FetchError in the loop: counted (:1448), then _run_one's notice (:1480-1484)
  | failedAdd       -- a FetchError in add's first check: counted, never told (add catches it, :1659)
  | refusedLadder   -- a 401/403 that moved the ladder: check returns None, errors untouched (:1443-1444)
  deriving DecidableEq, Repr

structure St where
  errs : Errs := .e0
  told : Bool := false        -- w.told_error
  toldN : Nat := 0            -- ghost: "I can't read" texts in this unbroken streak (saturated at 2)
  bad : Bool := false         -- ghost: "I can read X again" with no "I can't read" in the streak
  deriving DecidableEq, Repr

def fail (s : St) (tell : Bool) : St :=
  if tell && s.errs.succ == .e4 && !s.told then
    { s with errs := s.errs.succ, told := true, toldN := min 2 (s.toldN + 1) }
  else { s with errs := s.errs.succ }

def step (s : St) : Ev → St
  -- :1453 `if w.errors and w.told_error and self.notify is not None` -> the notice; :1456 resets all three.
  | .ok => { errs := .e0, told := false, toldN := 0,
             bad := s.bad || (s.errs != .e0 && s.told && s.toldN == 0) }
  | .failed => fail s true
  | .failedAdd => fail s false
  | .refusedLadder => s

/-- The property: at most one "I can't read" per unbroken error streak, and "I can read X again" only
after a streak that was told. -/
def Spec (s : St) : Bool := s.toldN ≤ 1 && !s.bad

def Inv (s : St) : Bool := Spec s && (s.told == (s.toldN == 1))

end Notice

open Notice in
theorem notice_inv_step (s : St) (e : Ev) : Notice.Inv s = true → Notice.Inv (Notice.step s e) = true := by
  rcases s with ⟨er, t, n, b⟩
  intro h
  have hn : n ≤ 1 := by
    unfold Notice.Inv Notice.Spec at h; simp only [Bool.and_eq_true, decide_eq_true_eq] at h; exact h.1.1
  have hn' : n = 0 ∨ n = 1 := by omega
  rcases hn' with rfl | rfl <;> cases er <;> cases t <;> cases b <;> cases e <;> revert h <;> decide

open Notice in
def Notice.run (evs : List Ev) : St := evs.foldl step {}

open Notice in
theorem notice_inv_run (evs : List Ev) : ∀ s : St, Notice.Inv s = true → Notice.Inv (evs.foldl Notice.step s) = true := by
  induction evs with
  | nil => intro s h; exact h
  | cons e rest ih => intro s h; exact ih _ (notice_inv_step s e h)

/-- Property 4, for the current code, over every sequence of checks: the owner hears "I can't read X" at
most once per unbroken error streak, and "I can read X again" only after a streak they were told of. -/
theorem notice_code_invariant (evs : List Notice.Ev) : Notice.Spec (Notice.run evs) = true := by
  have h := notice_inv_run evs {} (by decide)
  unfold Notice.Inv at h
  simp only [Bool.and_eq_true] at h
  exact h.1

/-! ## Part 3: the loop's wake-up

  `run` (:1512-1525): `wait = await self.tick()`, then `self._wake.clear()`, then
  `asyncio.wait_for(self._wake.wait(), timeout=min(wait, 300.0))`. `tick` returns
  `max(1.0, min(w.next_at for w in self.watches) - now)` (:1507), computed after its last await, over the
  list as it is then. `add` appends and sets the event with no await between (:1644-1646, and :1665-1667:
  releasing an asyncio.Lock does not suspend). `Event.wait` returns at once when the flag is already set.

  `late` is a ghost: some listed watch was appended after the loop's current wait was computed. The property:
  while the loop sleeps, a watch it did not count has the flag set, so the sleep ends at once and the next tick
  sees it; a watch it did count bounds the sleep by its own due time. Abstraction: time is left out (the
  bound is what `tick` returned); `tick` is assumed to return, which is what `_run_one` catching every
  exception (the fix to part 4) makes true of its checks. -/

namespace Wake

inductive Ev where
  | add       -- add appends and sets the event, while the loop ticks or sleeps
  | tickEnd   -- tick returns its wait over the current list; run clears the flag; wait_for starts
  | wake      -- the flag ends the sleep
  | timeout   -- the sleep runs out
  deriving DecidableEq, Repr

structure St where
  sleeping : Bool := false
  flag : Bool := false
  late : Bool := false
  deriving DecidableEq, Repr

def step (s : St) : Ev → St
  | .add => { s with flag := true, late := true }
  | .tickEnd => if s.sleeping then s else { sleeping := true, flag := false, late := false }
  | .wake => if s.sleeping && s.flag then { s with sleeping := false } else s
  | .timeout => if s.sleeping then { s with sleeping := false } else s

def Spec (s : St) : Bool := !s.sleeping || !s.late || s.flag

end Wake

open Wake in
theorem wake_inv_step (s : St) (e : Ev) : Wake.Spec s = true → Wake.Spec (Wake.step s e) = true := by
  rcases s with ⟨a, b, c⟩
  cases a <;> cases b <;> cases c <;> cases e <;> decide

open Wake in
def Wake.run (evs : List Ev) : St := evs.foldl step {}

open Wake in
theorem wake_inv_run (evs : List Ev) : ∀ s : St, Wake.Spec s = true → Wake.Spec (evs.foldl Wake.step s) = true := by
  induction evs with
  | nil => intro s h; exact h
  | cons e rest ih => intro s h; exact ih _ (wake_inv_step s e h)

/-- Property 5, for the current code, over every interleaving of adds with the loop's ticks and sleeps: a
watch added while the loop sleeps (or during a tick, after the tick's snapshot) never waits out a sleep that
did not count it. -/
theorem wake_code_invariant (evs : List Wake.Ev) : Wake.Spec (Wake.run evs) = true :=
  wake_inv_run evs {} rfl

/-! ## Part 4: a check that raises something other than FetchError

  `tick` (:1495-1504) walks the watches in `next_at` order. `_run_one` catches only FetchError (:1480), so
  anything else a reader or a rule raises (a model reply with a string for `choices[0].message`, AttributeError
  in `_reply_text`; a drop_pct baseline of 0, ZeroDivisionError in `evaluate`) leaves `tick` before `next_at`
  moved. `run` logs it and sleeps 60 s (:1519-1520), and the same watch, still the earliest, is first again.

  A watch is its outcome this tick (`fine`, `raises`, or `held` by the limiter, :1498-1501) and whether it is
  due. The list is in `next_at` order; a raising watch keeps its `next_at`, so a list that starts with one is
  in order again after the tick. The property: one tick leaves no due watch due (each is checked, backed off,
  or pushed back by the limiter). -/

namespace Starve

inductive Out where
  | fine | raises | held
  deriving DecidableEq, Repr

structure W where
  out : Out
  due : Bool
  checks : Nat := 0
  deriving DecidableEq, Repr

/-- The current tick: the exception leaves tick at the raising watch; it and every later watch are untouched. -/
def curTick : List W → List W
  | [] => []
  | w :: rest =>
      if !w.due then w :: curTick rest
      else match w.out with
        | .fine => { w with due := false, checks := w.checks + 1 } :: curTick rest
        | .held => { w with due := false } :: curTick rest
        | .raises => w :: rest

/-- The fixed tick: `_run_one` counts the raise as this watch's error and backs it off; tick goes on. -/
def fixTick : List W → List W
  | [] => []
  | w :: rest =>
      if !w.due then w :: fixTick rest
      else match w.out with
        | .fine => { w with due := false, checks := w.checks + 1 } :: fixTick rest
        | .held => { w with due := false } :: fixTick rest
        | .raises => { w with due := false } :: fixTick rest

def Spec (l : List W) : Bool := l.all (fun w => !w.due)

def iter (f : List W → List W) : Nat → List W → List W
  | 0, l => l
  | n + 1, l => iter f n (f l)

end Starve

open Starve in
/-- A raising watch due first and a quote watch due after it: however many ticks run, the quote watch is
never checked (the list is a fixed point of the current tick). -/
theorem starve_current_violates (n : Nat) :
    let l := [{ out := .raises, due := true : W }, { out := .fine, due := true : W }]
    iter curTick n l = l ∧ Starve.Spec (iter curTick n l) = false := by
  intro l
  have hfix : curTick l = l := by decide
  have : ∀ k, iter curTick k l = l := by
    intro k; induction k with
    | zero => rfl
    | succ k ih => simp only [iter, hfix, ih]
  exact ⟨this n, by rw [this n]; decide⟩

open Starve in
/-- With the fix, one tick leaves no watch due, whatever the list. -/
theorem starve_fixed_tick (l : List W) : Starve.Spec (fixTick l) = true := by
  induction l with
  | nil => rfl
  | cons w rest ih =>
      unfold fixTick
      unfold Starve.Spec at ih
      cases hd : w.due <;> cases ho : w.out <;> simp [Starve.Spec, hd, ih]

end Eve.WatchScheduler
