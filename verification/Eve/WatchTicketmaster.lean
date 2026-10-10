/-
  The Ticketmaster watch (bridge/src/eve_bridge/watch.py): `ticketmaster_reading` turns the Discovery
  API's events into a reading, and `evaluate` (condition "available") turns readings into texts.

  Ticketmaster's own status definitions (developer.ticketmaster.com, Discovery Feed, quoted in the
  function's docstring): onsale is within the public onsale timeframe; offsale is not yet reached or past it,
  and inventory may be available in an active presale; rescheduled is shown as on sale if there is
  inventory; postponed and canceled are not on sale. The properties, all proved for EVERY list of events
  and every `now` (no bound: they are proofs by induction, not an enumeration):

    1. `available_iff`: available iff some event is not canceled, not postponed, not an offsale event
       whose public sale has ended, and is onsale or rescheduled or has a presale window [start, end)
       holding now (a missing end is open-ended);
    2. `ended_is_invisible`: an offsale event whose public end has passed, put anywhere in the list,
       changes neither available, nor the dates listed, nor the dates on sale, nor the "Next:" note;
    3. `value_none_iff` / `value_some`: the value is None exactly when no event of the chosen pool
       (open events, else kept events, else all) has a positive price-range min; a value is always > 0
       (never 0) and the lowest positive min of the pool;
    4. `next_none_iff` / `next_is_earliest`: the "Next:" note names a sale start strictly after now, of a
       kept event, and none earlier exists among the kept events;
    5. over time, with `evaluate`: what the owner wants, decided here. A presale often needs a code the
       owner does not have; the public sale is the one anyone can use. So the owner wants to hear of each
       OPENING: a check that finds tickets buyable after one that found none. A presale, a gap, then the
       public sale is two texts; a presale that runs straight into the public sale is one (its text's note
       already says when the public sale opens). And each open stretch is told exactly once: by the add
       reply when that reply showed the sale open (evaluate's rule "the first reading never fires"), else
       by one text. `Spec` below is that last sentence.

  Found: the current code breaks 5 when the add reply showed NO reading. `add` keeps a watch whose first
  check failed ("the first check failed ...; I'll keep trying", watch.py:1664) or was queued (1647), with
  `checks == 0`. The next successful check from the loop is then "first" to `evaluate` (988, 1036-1037):
  it only arms, so a sale already open at that check is never texted, and the owner, who was never shown
  a reading, hears nothing until the sale closes and opens again (`current_violates`). The fix passes
  `shown` from `add` (1652, 1658) through `check` (1427, 1457) into `evaluate`, and only a shown first
  reading is quiet (`fixed_invariant`). Once the first reading was shown, current and fixed code are the
  same (`cur_eq_fix_after_shown`), and the texts are exactly the openings (`texts_are_openings`,
  `presale_gap_texts_twice`, `presale_into_public_texts_once`).

  Abstraction. Times are `Nat` (`_iso` gives float seconds; only `≤`/`<` against now and each other are
  used). A status code is one of six (anything not named is `other`; "cancelled" is `canceled`). A
  presale window is its start and end (a missing or unparsable time is `none`) and a name. A priceRanges
  entry is its `min` when it is a number, else `none`; Python's `min` over (value, currency) tuples is
  modelled by its value (the currency is ignored; the value is the smallest). `min(upcoming, key=...)`
  keeps the first of equal starts, as `earliest` does. The event's url, name and the note's words are
  not modelled. In Part 2 only successful checks are events: a failed check raises before `evaluate`
  (watch.py:1435-1451) and changes none of `checks`, `armed`, `last_available`. The one ticketmaster
  reading always has `available` set, so the `r.available is not None` guard is always true.
-/
namespace Eve.WatchTicketmaster

inductive Status where
  | onsale | offsale | rescheduled | postponed | canceled | other
  deriving DecidableEq, Repr

structure Win where
  name : Nat
  start : Option Nat
  stop : Option Nat
  deriving DecidableEq, Repr

inductive Name where
  | presale (n : Nat)
  | publicSale
  deriving DecidableEq, Repr

structure Ev where
  status : Status
  pubStart : Option Nat := none
  pubEnd : Option Nat := none
  presales : List Win := []
  mins : List (Option Int) := []
  deriving DecidableEq, Repr

/-- `if code in ("canceled", "cancelled", "postponed"): continue`, watch.py:1751-1752 -/
def skipped (e : Ev) : Bool := e.status == .canceled || e.status == .postponed

/-- `if code == "offsale" and end is not None and end <= now: continue`, watch.py:1756-1757 -/
def endedOffsale (now : Nat) (e : Ev) : Bool :=
  e.status == .offsale && (match e.pubEnd with | some x => decide (x ≤ now) | none => false)

/-- `s is not None and s <= now and (pe is None or now < pe)`, watch.py:1761 -/
def winOpen (now : Nat) (w : Win) : Bool :=
  match w.start with
  | some s => decide (s ≤ now) && (match w.stop with | none => true | some x => decide (now < x))
  | none => false

def presaleOpen (now : Nat) (e : Ev) : Bool := e.presales.any (winOpen now)

/-- `code in ("onsale", "rescheduled")`, watch.py:1762 -/
def statusOpen (e : Ev) : Bool := e.status == .onsale || e.status == .rescheduled

/-- `windows + [(start, None, "the public sale")]`, watch.py:1764 -/
def entries (e : Ev) : List (Option Nat × Name) :=
  e.presales.map (fun w => (w.start, Name.presale w.name)) ++ [(e.pubStart, Name.publicSale)]

def futureOf (now : Nat) (e : Ev) : Option Nat × Name → Option (Nat × Name × Ev)
  | (some t, n) => if now < t then some (t, n, e) else none
  | (none, _) => none

/-- `if s is not None and s > now: upcoming.append((s, name, e))`, watch.py:1765-1766 -/
def future (now : Nat) (e : Ev) : List (Nat × Name × Ev) := (entries e).filterMap (futureOf now e)

structure Acc where
  live : List Ev := []
  opens : List Ev := []
  upcoming : List (Nat × Name × Ev) := []

/-- One pass of the `for e in events` loop, watch.py:1749-1766. -/
def visit (now : Nat) (a : Acc) (e : Ev) : Acc :=
  if skipped e then a
  else if endedOffsale now e then a
  else
    let a1 : Acc := { a with live := a.live ++ [e] }
    let a2 : Acc := if statusOpen e || presaleOpen now e then { a1 with opens := a1.opens ++ [e] } else a1
    { a2 with upcoming := a2.upcoming ++ future now e }

def kept (now : Nat) (e : Ev) : Bool := !skipped e && !endedOffsale now e

theorem fold_visit (now : Nat) (evs : List Ev) : ∀ a : Acc,
    evs.foldl (visit now) a =
      { live := a.live ++ evs.filter (kept now),
        opens := a.opens ++ evs.filter (fun e => kept now e && (statusOpen e || presaleOpen now e)),
        upcoming := a.upcoming ++ evs.flatMap (fun e => if kept now e then future now e else []) } := by
  induction evs with
  | nil => intro a; simp
  | cons e rest ih =>
      intro a
      rw [List.foldl_cons, ih]
      by_cases hs : skipped e = true
      · simp [visit, hs, kept]
      · by_cases he : endedOffsale now e = true
        · simp [visit, hs, he, kept]
        · by_cases ho : (statusOpen e || presaleOpen now e) = true
          · simp [visit, hs, he, ho, kept]
          · simp [visit, hs, he, ho, kept]


/-- `lows`, watch.py:1769-1770: the numeric, positive mins of the pool. -/
def positives (pool : List Ev) : List Int :=
  pool.flatMap (fun e => e.mins.filterMap (fun m => match m with
    | some v => if 0 < v then some v else none
    | none => none))

/-- `min(lows)[0]`, watch.py:1771-1772 -/
def minOf : List Int → Option Int
  | [] => none
  | a :: l => some (l.foldl min a)

def earlier (b x : Nat × Name × Ev) : Nat × Name × Ev := if x.1 < b.1 then x else b

/-- `min(upcoming, key=lambda x: x[0])`, watch.py:1779 -/
def earliest : List (Nat × Name × Ev) → Option (Nat × Name × Ev)
  | [] => none
  | a :: l => some (l.foldl earlier a)

structure Reading where
  available : Bool
  value : Option Int
  listed : Nat
  onSale : Nat
  next : Option (Nat × Name × Ev)
  deriving DecidableEq, Repr

/-- `pool = open_events or live or events`, watch.py:1768 -/
def poolOf (evs : List Ev) (a : Acc) : List Ev :=
  if !a.opens.isEmpty then a.opens else if !a.live.isEmpty then a.live else evs

/-- `ticketmaster_reading(events, now)`, watch.py:1734-1784: the empty list returns early (1744-1747);
`listed` is `len(shown)` with `shown = live`, `onSale` is `len(open_events)` (1776-1777). -/
def reading (evs : List Ev) (now : Nat) : Reading :=
  if evs.isEmpty then { available := false, value := none, listed := 0, onSale := 0, next := none }
  else
    let a := evs.foldl (visit now) {}
    { available := !a.opens.isEmpty, value := minOf (positives (poolOf evs a)),
      listed := a.live.length, onSale := a.opens.length, next := earliest a.upcoming }

/-! ### The spec, in Ticketmaster's words -/

def Ended (now : Nat) (e : Ev) : Prop := e.status = .offsale ∧ ∃ x, e.pubEnd = some x ∧ x ≤ now

def WinOpen (now : Nat) (w : Win) : Prop := ∃ s, w.start = some s ∧ s ≤ now ∧ ∀ x, w.stop = some x → now < x

def OnSale (now : Nat) (e : Ev) : Prop :=
  e.status ≠ .canceled ∧ e.status ≠ .postponed ∧ ¬ Ended now e ∧
    (e.status = .onsale ∨ e.status = .rescheduled ∨ ∃ w ∈ e.presales, WinOpen now w)

theorem kept_iff (now : Nat) (e : Ev) :
    kept now e = true ↔ e.status ≠ .canceled ∧ e.status ≠ .postponed ∧ ¬ Ended now e := by
  rcases e with ⟨st, ps, pe, pr, mi⟩
  cases st <;> cases pe <;> simp [kept, skipped, endedOffsale, Ended]

theorem winOpen_iff (now : Nat) (w : Win) : winOpen now w = true ↔ WinOpen now w := by
  rcases w with ⟨n, s, t⟩
  cases s <;> cases t <;> simp [winOpen, WinOpen]

theorem open_iff (now : Nat) (e : Ev) :
    (kept now e && (statusOpen e || presaleOpen now e)) = true ↔ OnSale now e := by
  have hk := kept_iff now e
  have hw : presaleOpen now e = true ↔ ∃ w ∈ e.presales, WinOpen now w := by
    simp [presaleOpen, winOpen_iff]
  rcases e with ⟨st, ps, pe, pr, mi⟩
  simp only [OnSale, Bool.and_eq_true, Bool.or_eq_true, hk, ← hw]
  cases st <;> simp [statusOpen]


/-! ### Property 1: available -/

theorem filter_nonempty {α} (p : α → Bool) (l : List α) :
    (!(l.filter p).isEmpty) = true ↔ ∃ x ∈ l, p x = true := by
  simp [List.filter_eq_nil_iff]

theorem available_iff (evs : List Ev) (now : Nat) :
    (reading evs now).available = true ↔ ∃ e ∈ evs, OnSale now e := by
  cases evs with
  | nil => simp [reading]
  | cons e0 rest =>
      simp only [reading, List.isEmpty_cons, Bool.false_eq_true, ↓reduceIte, fold_visit, List.nil_append,
        filter_nonempty]
      constructor
      · rintro ⟨e, he, ho⟩; exact ⟨e, he, (open_iff now e).mp ho⟩
      · rintro ⟨e, he, ho⟩; exact ⟨e, he, (open_iff now e).mpr ho⟩

/-! ### Property 2: an offsale event whose public sale has ended is not there at all -/

theorem ended_not_kept (now : Nat) (e : Ev) (h : Ended now e) : kept now e = false := by
  cases hk : kept now e with
  | false => rfl
  | true => exact absurd h ((kept_iff now e).mp hk).2.2

theorem visit_not_kept (now : Nat) (a : Acc) (e : Ev) (h : kept now e = false) : visit now a e = a := by
  unfold kept at h
  cases hs : skipped e <;> cases he : endedOffsale now e <;> simp_all [visit]

/-- Put an ended offsale event anywhere in the API's list: whether tickets are available, the dates
listed, the dates on sale and the "Next:" note are exactly what they are without it. -/
theorem ended_is_invisible (now : Nat) (evs1 evs2 : List Ev) (e : Ev) (h : Ended now e) :
    let r := reading (evs1 ++ e :: evs2) now
    let r' := reading (evs1 ++ evs2) now
    r.available = r'.available ∧ r.listed = r'.listed ∧ r.onSale = r'.onSale ∧ r.next = r'.next := by
  have hk := ended_not_kept now e h
  cases hne : (evs1 ++ evs2).isEmpty with
  | true =>
      have h1 : evs1 = [] ∧ evs2 = [] := by simpa using hne
      obtain ⟨rfl, rfl⟩ := h1
      simp [reading, visit_not_kept now _ e hk, earliest]
  | false =>
      simp [reading, fold_visit, List.foldl_append, visit_not_kept now _ e hk, hne]


/-! ### Property 3: the price -/

/-- The pool the price is read from (watch.py `pool = open_events or live or events`). -/
def Pool (evs : List Ev) (now : Nat) : List Ev :=
  let o := evs.filter (fun e => kept now e && (statusOpen e || presaleOpen now e))
  let l := evs.filter (kept now)
  if !o.isEmpty then o else if !l.isEmpty then l else evs

theorem reading_value (evs : List Ev) (now : Nat) :
    (reading evs now).value = minOf (positives (Pool evs now)) := by
  cases evs with
  | nil => simp [reading, Pool, positives, minOf]
  | cons e0 rest =>
      simp only [reading, List.isEmpty_cons, Bool.false_eq_true, ↓reduceIte]
      rw [fold_visit]
      simp only [poolOf, Pool, List.nil_append]

theorem mem_positives (pool : List Ev) (v : Int) :
    v ∈ positives pool ↔ ∃ e ∈ pool, some v ∈ e.mins ∧ 0 < v := by
  simp only [positives, List.mem_flatMap, List.mem_filterMap]
  constructor
  · rintro ⟨e, he, m, hm, hv⟩
    cases m with
    | none => simp at hv
    | some w =>
        by_cases hw : 0 < w
        · simp [hw] at hv; subst hv; exact ⟨e, he, hm, hw⟩
        · simp [hw] at hv
  · rintro ⟨e, he, hm, hv⟩
    exact ⟨e, he, some v, hm, by simp [hv]⟩

theorem foldl_min (l : List Int) : ∀ a : Int,
    (l.foldl min a = a ∨ l.foldl min a ∈ l) ∧ l.foldl min a ≤ a ∧ ∀ x ∈ l, l.foldl min a ≤ x := by
  induction l with
  | nil => intro a; simp
  | cons b l ih =>
      intro a
      obtain ⟨h1, h2, h3⟩ := ih (min a b)
      simp only [List.foldl_cons, List.mem_cons, forall_eq_or_imp]
      refine ⟨?_, by omega, by omega, h3⟩
      rcases h1 with h | h
      · rw [h]; by_cases hab : a ≤ b
        · left; omega
        · right; left; omega
      · right; right; exact h

theorem minOf_none (l : List Int) : minOf l = none ↔ l = [] := by
  cases l <;> simp [minOf]

theorem minOf_some (l : List Int) (v : Int) (h : minOf l = some v) : v ∈ l ∧ ∀ x ∈ l, v ≤ x := by
  cases l with
  | nil => simp [minOf] at h
  | cons a rest =>
      simp only [minOf, Option.some.injEq] at h
      subst h
      obtain ⟨h1, h2, h3⟩ := foldl_min rest a
      refine ⟨?_, ?_⟩
      · rcases h1 with h | h
        · rw [h]; exact List.mem_cons_self
        · exact List.mem_cons_of_mem _ h
      · intro x hx
        rcases List.mem_cons.mp hx with rfl | hx
        · exact h2
        · exact h3 x hx

/-- No positive price-range min in the pool: the value is unknown (None), never 0. -/
theorem value_none_iff (evs : List Ev) (now : Nat) :
    (reading evs now).value = none ↔ ∀ e ∈ Pool evs now, ∀ v, some v ∈ e.mins → v ≤ 0 := by
  rw [reading_value, minOf_none, List.eq_nil_iff_forall_not_mem]
  constructor
  · intro h e he v hv
    have := h v
    rw [mem_positives] at this
    by_cases hv0 : 0 < v
    · exact absurd ⟨e, he, hv, hv0⟩ this
    · omega
  · intro h v hv
    obtain ⟨e, he, hm, hpos⟩ := (mem_positives _ v).mp hv
    have := h e he v hm
    omega

/-- A value is always a positive min from the pool, and the lowest of them. -/
theorem value_some (evs : List Ev) (now : Nat) (v : Int) (h : (reading evs now).value = some v) :
    0 < v ∧ (∃ e ∈ Pool evs now, some v ∈ e.mins) ∧
      ∀ e ∈ Pool evs now, ∀ v', some v' ∈ e.mins → 0 < v' → v ≤ v' := by
  rw [reading_value] at h
  obtain ⟨hm, hl⟩ := minOf_some _ _ h
  obtain ⟨e, he, hv, hpos⟩ := (mem_positives _ v).mp hm
  exact ⟨hpos, ⟨e, he, hv⟩, fun e' he' v' hv' hp => hl v' ((mem_positives _ v').mpr ⟨e', he', hv', hp⟩)⟩


/-! ### Property 4: the "Next:" note -/

/-- A sale start the note may name: a presale window's or the public sale's start, strictly after now,
of an event that was kept (not canceled, not postponed, not an ended offsale). -/
def Cand (now : Nat) (evs : List Ev) (t : Nat) (n : Name) (e : Ev) : Prop :=
  e ∈ evs ∧ kept now e = true ∧ now < t ∧ (some t, n) ∈ entries e

def upcomingOf (evs : List Ev) (now : Nat) : List (Nat × Name × Ev) :=
  evs.flatMap (fun e => if kept now e then future now e else [])

theorem reading_next (evs : List Ev) (now : Nat) : (reading evs now).next = earliest (upcomingOf evs now) := by
  cases evs with
  | nil => simp [reading, upcomingOf, earliest]
  | cons e0 rest =>
      simp only [reading, List.isEmpty_cons, Bool.false_eq_true, ↓reduceIte]
      rw [fold_visit]
      simp only [upcomingOf, List.nil_append]

theorem futureOf_some (now : Nat) (e : Ev) (x : Option Nat × Name) (u : Nat × Name × Ev) :
    futureOf now e x = some u ↔ x = (some u.1, u.2.1) ∧ u.2.2 = e ∧ now < u.1 := by
  obtain ⟨t, n, e'⟩ := u
  rcases x with ⟨_ | s, m⟩
  · simp [futureOf]
  · by_cases h : now < s
    · simp only [futureOf, h, ↓reduceIte, Option.some.injEq, Prod.mk.injEq]
      constructor
      · rintro ⟨rfl, rfl, rfl⟩; exact ⟨⟨rfl, rfl⟩, rfl, h⟩
      · rintro ⟨⟨rfl, rfl⟩, rfl, _⟩; exact ⟨rfl, rfl, rfl⟩
    · simp only [futureOf, h, ↓reduceIte, reduceCtorEq, false_iff, not_and, Prod.mk.injEq,
        Option.some.injEq]
      rintro ⟨rfl, rfl⟩ _; exact h

theorem mem_upcoming (evs : List Ev) (now : Nat) (u : Nat × Name × Ev) :
    u ∈ upcomingOf evs now ↔ Cand now evs u.1 u.2.1 u.2.2 := by
  obtain ⟨t, n, e⟩ := u
  simp only [upcomingOf, List.mem_flatMap, Cand]
  constructor
  · rintro ⟨e', he', hu⟩
    cases hk : kept now e' with
    | false => simp [hk] at hu
    | true =>
        simp only [hk, ↓reduceIte, future, List.mem_filterMap, futureOf_some] at hu
        obtain ⟨x, hx, rfl, rfl, hlt⟩ := hu
        exact ⟨he', hk, hlt, hx⟩
  · rintro ⟨he, hk, hlt, hx⟩
    refine ⟨e, he, ?_⟩
    simp only [hk, ↓reduceIte, future, List.mem_filterMap, futureOf_some]
    exact ⟨_, hx, by simp, by simp, hlt⟩

theorem foldl_earlier (l : List (Nat × Name × Ev)) : ∀ a,
    (l.foldl earlier a = a ∨ l.foldl earlier a ∈ l) ∧ (l.foldl earlier a).1 ≤ a.1 ∧
      ∀ x ∈ l, (l.foldl earlier a).1 ≤ x.1 := by
  induction l with
  | nil => intro a; simp
  | cons b l ih =>
      intro a
      obtain ⟨h1, h2, h3⟩ := ih (earlier a b)
      have hab : (earlier a b).1 ≤ a.1 ∧ (earlier a b).1 ≤ b.1 ∧ (earlier a b = a ∨ earlier a b = b) := by
        unfold earlier; by_cases h : b.1 < a.1 <;> simp [h] <;> omega
      simp only [List.foldl_cons, List.mem_cons, forall_eq_or_imp]
      refine ⟨?_, by omega, by omega, h3⟩
      rcases h1 with h | h
      · rw [h]; rcases hab.2.2 with h' | h'
        · left; exact h'
        · right; left; exact h'
      · right; right; exact h

theorem earliest_none (l : List (Nat × Name × Ev)) : earliest l = none ↔ l = [] := by
  cases l <;> simp [earliest]

theorem earliest_some (l : List (Nat × Name × Ev)) (u : Nat × Name × Ev) (h : earliest l = some u) :
    u ∈ l ∧ ∀ x ∈ l, u.1 ≤ x.1 := by
  cases l with
  | nil => simp [earliest] at h
  | cons a rest =>
      simp only [earliest, Option.some.injEq] at h
      subst h
      obtain ⟨h1, h2, h3⟩ := foldl_earlier rest a
      refine ⟨?_, ?_⟩
      · rcases h1 with h | h
        · rw [h]; exact List.mem_cons_self
        · exact List.mem_cons_of_mem _ h
      · intro x hx
        rcases List.mem_cons.mp hx with rfl | hx
        · exact h2
        · exact h3 x hx

/-- No "Next:" exactly when no kept event has a sale start after now. -/
theorem next_none_iff (evs : List Ev) (now : Nat) :
    (reading evs now).next = none ↔ ∀ t n e, ¬ Cand now evs t n e := by
  rw [reading_next, earliest_none, List.eq_nil_iff_forall_not_mem]
  constructor
  · intro h t n e hc; exact h (t, n, e) ((mem_upcoming evs now (t, n, e)).mpr hc)
  · intro h u hu; exact h u.1 u.2.1 u.2.2 ((mem_upcoming evs now u).mp hu)

/-- The "Next:" note names a sale start that is strictly in the future, of a kept event (never an ended
offsale one), and no kept event has an earlier one. -/
theorem next_is_earliest (evs : List Ev) (now t : Nat) (n : Name) (e : Ev)
    (h : (reading evs now).next = some (t, n, e)) :
    Cand now evs t n e ∧ ¬ Ended now e ∧ ∀ t' n' e', Cand now evs t' n' e' → t ≤ t' := by
  rw [reading_next] at h
  obtain ⟨hm, hl⟩ := earliest_some _ _ h
  have hc := (mem_upcoming evs now _).mp hm
  refine ⟨hc, ((kept_iff now e).mp hc.2.1).2.2, ?_⟩
  intro t' n' e' hc'
  exact hl (t', n', e') ((mem_upcoming evs now (t', n', e')).mpr hc')


/-! ## Part 2: the texts over time -/

/-- How `add`'s first check went. -/
inductive AddEv where
  | shown (b : Bool)   -- it read availability `b`, and the add reply showed it ("available" / "not available")
  | later              -- it failed (a 5xx, the network) or was queued: the reply showed no reading
  deriving DecidableEq, Repr

structure W where
  checks : Nat := 0                -- w.checks
  armed : Bool := true             -- w.armed (Watch default True)
  lastAvail : Option Bool := none  -- w.last_available
  informed : Nat := 0              -- ghost: how often the owner was told of the current open stretch
  texts : Nat := 0                 -- ghost: texts sent
  deriving DecidableEq, Repr

/-- Each open stretch (checks in a row that found tickets buyable) is told to the owner exactly once:
by the add reply when it showed the sale open, else by one text. -/
def Spec (s : W) : Bool := s.lastAvail != some true || s.informed == 1

/-- The ghost bookkeeping of one successful reading `b`: `pre` is the state before, `post` the code's
state after, `told` whether the owner was told now (the reply showed it, or a text went). -/
def ghost (pre post : W) (b told alert : Bool) : W :=
  let fresh := b && pre.lastAvail != some true
  { post with lastAvail := some b,
              informed := if !b then 0 else (if fresh then 0 else pre.informed) + (if told then 1 else 0),
              texts := pre.texts + (if alert then 1 else 0) }

/-! ### The current code -/

/-- `evaluate`'s "available" branch as it is (watch.py:988-990, 1035-1043): every first reading only arms. -/
def Cur.eval (s : W) (b : Bool) : W × Bool :=
  let s1 := { s with checks := s.checks + 1 }
  if s.checks == 0 then ({ s1 with armed := !b }, false)
  else if b && s.armed then ({ s1 with armed := false }, true)
  else if !b then ({ s1 with armed := true }, false)
  else (s1, false)

/-- `add`, watch.py:1630-1679: the first check (1652) is shown in the reply; a failed or queued one keeps
the watch with `checks == 0` (1644-1648, 1664). -/
def Cur.add : AddEv → W
  | .shown b => ghost {} (Cur.eval {} b).1 b b false
  | .later => {}

/-- A check from the loop (`tick` -> `_run_one` -> `check`): a successful read of availability `b`. -/
def Cur.read (s : W) (b : Bool) : W :=
  let (s', alert) := Cur.eval s b
  ghost s s' b alert alert

def Cur.run (a : AddEv) (reads : List Bool) : W := reads.foldl Cur.read (Cur.add a)

/-- The owner adds the watch while the API is down (or the first check is queued); the next check finds
a presale open: the owner is never told. It stays silent for as long as the sale stays open. -/
theorem current_violates :
    let s := Cur.run .later [true]
    s.lastAvail = some true ∧ s.informed = 0 ∧ s.texts = 0 ∧ Spec s = false ∧
      (Cur.run .later [true, true, true]).texts = 0 := by
  decide

/-! ### The fixed code -/

/-- `evaluate(w, r, shown=...)` (fixed): a first reading only arms when the add reply showed it. -/
def Fix.eval (s : W) (b shown : Bool) : W × Bool :=
  let s1 := { s with checks := s.checks + 1 }
  if s.checks == 0 && shown then ({ s1 with armed := !b }, false)
  else if b && s.armed then ({ s1 with armed := false }, true)
  else if !b then ({ s1 with armed := true }, false)
  else (s1, false)

def Fix.add : AddEv → W
  | .shown b => ghost {} (Fix.eval {} b true).1 b b false
  | .later => {}

def Fix.read (s : W) (b : Bool) : W :=
  let (s', alert) := Fix.eval s b false
  ghost s s' b alert alert

def Fix.run (a : AddEv) (reads : List Bool) : W := reads.foldl Fix.read (Fix.add a)

def Inv (s : W) : Bool := Spec s && (s.armed == (s.lastAvail != some true))

theorem inv_add (a : AddEv) : Inv (Fix.add a) = true := by
  cases a with
  | shown b => cases b <;> decide
  | later => decide

theorem inv_read (s : W) (b : Bool) (h : Inv s = true) : Inv (Fix.read s b) = true := by
  rcases s with ⟨ch, ar, la, inf, tx⟩
  cases ar <;> cases b <;> rcases la with _ | _ | _ <;> cases ch <;>
    simp_all [Inv, Spec, Fix.read, Fix.eval, ghost]

theorem inv_run (reads : List Bool) : ∀ s : W, Inv s = true → Inv (reads.foldl Fix.read s) = true := by
  induction reads with
  | nil => intro s h; exact h
  | cons b rest ih => intro s h; exact ih _ (inv_read s b h)

/-- Whatever the first check did and whatever the checks after it read, every open stretch is told to
the owner exactly once. -/
theorem fixed_invariant (a : AddEv) (reads : List Bool) : Spec (Fix.run a reads) = true := by
  have h := inv_run reads (Fix.add a) (inv_add a)
  unfold Inv at h
  simp only [Bool.and_eq_true] at h
  exact h.1

theorem fixed_replays_counterexample :
    let s := Fix.run .later [true]
    s.texts = 1 ∧ Spec s = true ∧ (Fix.run .later [true, true, true]).texts = 1 := by
  decide


/-! ## Part 3: one text per opening, with the reading in the loop -/

/-- The openings in a run of readings: a reading that finds tickets buyable after one that did not. -/
def openings (prev : Bool) : List Bool → Nat
  | [] => 0
  | b :: rest => (if b && !prev then 1 else 0) + openings b rest

theorem fix_texts (reads : List Bool) : ∀ s : W, Inv s = true →
    (reads.foldl Fix.read s).texts = s.texts + openings (s.lastAvail == some true) reads := by
  induction reads with
  | nil => intro s _; simp [openings]
  | cons b rest ih =>
      intro s h
      rw [List.foldl_cons, ih _ (inv_read s b h)]
      rcases s with ⟨ch, ar, la, inf, tx⟩
      cases ar <;> cases b <;> rcases la with _ | _ | _ <;> cases ch <;>
        simp_all [Inv, Spec, Fix.read, Fix.eval, ghost, openings] <;> omega

/-- The current code is the fixed code once the first reading was shown in the add reply. -/
theorem cur_eq_fix_after_shown (b0 : Bool) (reads : List Bool) :
    Cur.run (.shown b0) reads = Fix.run (.shown b0) reads := by
  have step : ∀ s : W, s.checks ≠ 0 → ∀ b, Cur.read s b = Fix.read s b ∧ (Fix.read s b).checks ≠ 0 := by
    intro s hs b
    rcases s with ⟨ch, ar, la, inf, tx⟩
    cases ch with
    | zero => exact absurd rfl hs
    | succ n => cases ar <;> cases b <;> simp [Cur.read, Fix.read, Cur.eval, Fix.eval, ghost]
  have go : ∀ (reads : List Bool) (s : W), s.checks ≠ 0 → reads.foldl Cur.read s = reads.foldl Fix.read s := by
    intro reads
    induction reads with
    | nil => intro s _; rfl
    | cons b rest ih =>
        intro s hs
        simp only [List.foldl_cons, (step s hs b).1]
        exact ih _ (step s hs b).2
  have hadd : Cur.add (.shown b0) = Fix.add (.shown b0) := by cases b0 <;> decide
  unfold Cur.run Fix.run
  rw [hadd]
  exact go reads _ (by cases b0 <;> decide)

/-- One event on Ticketmaster: a presale window `[a, b)` and the public sale from `c`, its status
offsale before `c` and onsale from `c` (the Discovery Feed's definitions). -/
def schedEv (a b c t : Nat) : Ev :=
  { status := if c ≤ t then .onsale else .offsale, pubStart := some c,
    presales := [{ name := 0, start := some a, stop := some b }] }

/-- Buyable at `t`, in the owner's words: in the presale window or at or after the public sale. -/
def buyable (a b c t : Nat) : Bool := (decide (a ≤ t) && decide (t < b)) || decide (c ≤ t)

theorem sched_available (a b c t : Nat) : (reading [schedEv a b c t] t).available = buyable a b c t := by
  by_cases hc : c ≤ t
  · simp [reading, schedEv, visit, skipped, endedOffsale, statusOpen, buyable, hc]
  · by_cases hw : a ≤ t ∧ t < b
    · simp [reading, schedEv, visit, skipped, endedOffsale, statusOpen, presaleOpen, winOpen, buyable, hc, hw]
    · have : ¬ a ≤ t ∨ ¬ t < b := by omega
      rcases this with h | h <;>
        simp [reading, schedEv, visit, skipped, endedOffsale, statusOpen, presaleOpen, winOpen, buyable, hc, h]

def checksAt (a b c : Nat) (ts : List Nat) : List Bool := ts.map (fun t => (reading [schedEv a b c t] t).available)

/-- Property 5 on the current code: with the first check shown at `t0` and later checks at `ts`, the
owner gets one text per opening of the sale, whatever the windows and the check times. -/
theorem texts_are_openings (a b c t0 : Nat) (ts : List Nat) :
    (Cur.run (.shown (buyable a b c t0)) (checksAt a b c ts)).texts =
      openings (buyable a b c t0) (ts.map (buyable a b c)) := by
  rw [cur_eq_fix_after_shown]
  unfold Fix.run
  rw [fix_texts _ _ (inv_add _)]
  have hmap : checksAt a b c ts = ts.map (buyable a b c) := by
    unfold checksAt; congr 1; funext t; exact sched_available a b c t
  rw [hmap]
  cases buyable a b c t0 <;> simp [Fix.add, Fix.eval, ghost]

/-- A presale 1..2, a gap, the public sale from 4, checked each hour 1..5: two texts. -/
theorem presale_gap_texts_twice : (Cur.run (.shown (buyable 1 2 4 0)) (checksAt 1 2 4 [1, 2, 3, 4, 5])).texts = 2 := by
  decide

/-- A presale 1..3 that runs into the public sale at 3: one text. -/
theorem presale_into_public_texts_once :
    (Cur.run (.shown (buyable 1 3 3 0)) (checksAt 1 3 3 [1, 2, 3, 4, 5])).texts = 1 := by
  decide

end Eve.WatchTicketmaster
