/-
# Forget vs the dream (bridge/src/eve_bridge/{memory,records,dream,mem0_memory}.py)

`Memory.forget_apply` redacts the transcripts, scrubs the records (and commits), forgets the matching mem0
memories, and squashes the records' git history. The nightly dream runs beside it on a worker thread:
it reads the day's transcript and the records, calls a model for minutes with the records lock released,
then writes the model's answer and commits it. mem0's `ingest_day` reads a conversation, then adds it.

The property: **once a forget has completed, no later state holds a forgotten line in the records, their
git history, or the mem0 index** — for every interleaving of the forget with any number of dream reads,
dream writes, index reads, index adds and new things the owner says. A line the owner says after the
forget's redaction is new input, not a forgotten line: it carries `born = true` and is allowed.

`current_violates` shows the code before the fix broke it twice: a dream whose model call is out while
the forget runs writes the pre-forget words back into the records and commits them after the squash; an
ingest that read a conversation before the forget adds it to mem0 after the forget.
`fixed_invariant` proves the guarded code upholds the property for every event list and every forgotten
predicate, by induction with a strengthened invariant (`Inv`).

Trusted assumptions (stated, not proved):
* Each event below is atomic in the real code: the records steps run under `records._locked` (a thread
  RLock plus an flock on the folder), the mem0 steps under `OwnerMemory._lock`, and a transcript read
  or redaction of one day under the transcripts' own writer lock and an atomic rename.
* The model's answer holds only lines from its input (worst case: all of them). A model that invents
  the forgotten words from nothing is outside what any store-side fix can prevent.
* One forget per trace, with any predicate `m`. Several forgets compose, since each one is this trace
  started from whatever state the previous one left (the theorem quantifies over the initial stores).
* Lines are compared by value; the index's delete-by-id is modelled as a value filter. `current` models
  the index forget as atomic, which is stronger than the real split `find` then `forget`
  (memory.py:449-450 before the fix), so a violation of the model is a violation of the code.
-/

namespace Eve.Forget

/-- A line: its text, and whether the owner said it after the forget's redaction (new input). -/
abbrev Line := Nat × Bool

structure St where
  tx   : List Line      -- transcripts/
  recs : List Line      -- records/ working tree
  git  : List Line      -- every line any records commit still reachable holds
  idx  : List Line      -- mem0/
  gen  : Nat            -- records forget generation (records.py `_generation`: `.forgets` + `_FORGETS`)
  igen : Nat            -- mem0 forget generation (mem0_memory.py `OwnerMemory._forgets`)
  fpc  : Nat            -- how far the forget got: 0 not begun … 4 returned ok
  dpin : Nat            -- the generation the dream pinned
  dbuf : List Line      -- what the dream read and handed the model
  ipin : Nat            -- the generation the ingest pinned
  ibuf : List Line      -- what the ingest read
  deriving Repr

inductive Ev
  | say (w : Nat)       -- the owner says something (transcripts.append)
  | fRedact             -- forget: transcripts
  | fRecords            -- forget: records (+ commit)
  | fIndex              -- forget: mem0
  | fSquash             -- forget: squash the records history; forget_apply returns ok
  | dPin                -- dream: a night begins
  | dReadDay            -- dream: reads the transcript day
  | dReadRecs           -- dream: reads the records (reconcile_day / consolidate)
  | dWrite              -- dream: writes the model's answer and commits
  | iPin                -- ingest_day begins
  | iRead               -- ingest reads a conversation
  | iAdd                -- ingest adds it to mem0
  deriving Repr

/-- The forgotten predicate `m` holds and the line is not new input. -/
def bad (m : Nat → Bool) (x : Line) : Bool := m x.1 && !x.2

def scrub (m : Nat → Bool) (l : List Line) : List Line := l.filter (fun x => !bad m x)

/-- One step. `fixed = false` is the code before the fix, `fixed = true` after it. -/
def step (fixed : Bool) (m : Nat → Bool) (s : St) : Ev → St
  -- transcripts.py `append`: said after the redaction ⇒ new input
  | .say w => { s with tx := s.tx ++ [(w, decide (1 ≤ s.fpc))] }
  -- memory.py:434 `self.transcripts.redact(pending.query)`
  | .fRedact => if s.fpc = 0 then { s with tx := scrub m s.tx, fpc := 1 } else s
  -- memory.py:438 `rec.forget_lines(self.cfg, pred)`; records.py:1086-1110 under `_locked`: `_bump`
  -- (fix, records.py:1088) + scrub + commit "forget: N lines". Before the fix the bump did not exist;
  -- a counter nobody reads changes nothing, so the same step models both.
  | .fRecords =>
      if s.fpc = 1 then
        let r := scrub m s.recs
        { s with recs := r, git := s.git ++ r, gen := s.gen + 1, fpc := 2 }
      else s
  -- memory.py:445 `forget_matching(pred)` → mem0_memory.py:462-485, one hold of `_lock`: bump + list +
  -- delete (fix). Before: memory.py:449-450 `find` then `forget` (see the assumptions).
  | .fIndex =>
      if s.fpc = 2 then { s with idx := scrub m s.idx, igen := s.igen + 1, fpc := 3 } else s
  -- memory.py:462 `rec.squash_history(repo)`: the tree becomes the only commit
  | .fSquash => if s.fpc = 3 then { s with git := s.recs, fpc := 4 } else s
  -- dream.py:218 `records.forgets_pinned(self.cfg)` (fix); before the fix the night just began
  | .dPin => { s with dpin := s.gen, dbuf := [] }
  -- dream.py:235 `self.transcripts.day_text(day)` (dream.py:224 before the fix)
  | .dReadDay => { s with dbuf := s.dbuf ++ s.tx }
  -- records.py:902-908 (reconcile_day) and 953-956 (consolidate) under `_locked`
  -- (records.py:844-849 and 891-893 before the fix)
  | .dReadRecs => { s with dbuf := s.dbuf ++ s.recs }
  -- records.py:922-927 under `_locked`: `if _generation(folder) != since: return None` (fix), then
  -- `apply_day` + `commit`; consolidate likewise at records.py:970-973.
  -- Before the fix: records.py:863-865 and 907, no check.
  | .dWrite =>
      if !fixed || s.gen = s.dpin then { s with recs := s.recs ++ s.dbuf, git := s.git ++ s.dbuf } else s
  -- mem0_memory.py:367-368 `since = self._forgets` under `_lock` (fix)
  | .iPin => { s with ipin := s.igen, ibuf := [] }
  -- mem0_memory.py:372 `transcripts.conv_lines(info.conv)` (369 before the fix)
  | .iRead => { s with ibuf := s.ibuf ++ s.tx }
  -- mem0_memory.py:377-385 under `_lock`: `if self._forgets != since: return added` (fix), `mem.add`
  -- (mem0_memory.py:375-379 before the fix, no check)
  | .iAdd => if !fixed || s.igen = s.ipin then { s with idx := s.idx ++ s.ibuf } else s

def run (fixed : Bool) (m : Nat → Bool) (s : St) (es : List Ev) : St := es.foldl (step fixed m) s

/-! ## The code before the fix -/

def isSeven (w : Nat) : Bool := w == 7

/-- The owner said line 7 once; nothing else anywhere. -/
def init : St :=
  { tx := [(7, false)], recs := [], git := [], idx := [], gen := 0, igen := 0, fpc := 0,
    dpin := 0, dbuf := [], ipin := 0, ibuf := [] }

/-- The dream reads the day, the whole forget runs during its model call, then the dream writes. -/
def dreamTrace : List Ev := [.dPin, .dReadDay, .dReadRecs, .fRedact, .fRecords, .fIndex, .fSquash, .dWrite]

/-- The index reads a conversation, the whole forget runs, then the index adds what it read. -/
def ingestTrace : List Ev := [.iPin, .iRead, .fRedact, .fRecords, .fIndex, .fSquash, .iAdd]

theorem current_violates :
    (run false isSeven init dreamTrace).fpc = 4 ∧
    (run false isSeven init dreamTrace).recs.any (bad isSeven) = true ∧
    (run false isSeven init dreamTrace).git.any (bad isSeven) = true ∧
    (run false isSeven init ingestTrace).fpc = 4 ∧
    (run false isSeven init ingestTrace).idx.any (bad isSeven) = true := by
  decide

/-- The same two interleavings on the fixed code leave nothing behind (a sanity check of the model). -/
theorem fixed_blocks_counterexamples :
    (run true isSeven init dreamTrace).recs.any (bad isSeven) = false ∧
    (run true isSeven init dreamTrace).git.any (bad isSeven) = false ∧
    (run true isSeven init ingestTrace).idx.any (bad isSeven) = false := by
  decide

/-! ## The fix, for every interleaving -/

def clean (m : Nat → Bool) (l : List Line) : Prop := ∀ x, x ∈ l → bad m x = false

theorem clean_nil (m : Nat → Bool) : clean m [] := by
  intro x hx; cases hx

theorem clean_append {m : Nat → Bool} {a b : List Line} (ha : clean m a) (hb : clean m b) :
    clean m (a ++ b) := by
  intro x hx
  rcases List.mem_append.mp hx with h | h
  · exact ha x h
  · exact hb x h

theorem clean_scrub (m : Nat → Bool) (l : List Line) : clean m (scrub m l) := by
  intro x hx
  have h := (List.mem_filter.mp hx).2
  cases hb : bad m x
  · rfl
  · rw [hb] at h; cases h

theorem clean_new (m : Nat → Bool) (w : Nat) : clean m [(w, true)] := by
  intro x hx
  rw [List.mem_singleton] at hx
  subst hx
  simp [bad]

/-- The strengthened invariant. -/
structure Inv (m : Nat → Bool) (s : St) : Prop where
  dle   : s.dpin ≤ s.gen
  ile   : s.ipin ≤ s.igen
  fle   : s.fpc ≤ 4
  tx    : 1 ≤ s.fpc → clean m s.tx
  recs  : 2 ≤ s.fpc → clean m s.recs
  dbuf  : 2 ≤ s.fpc → s.dpin = s.gen → clean m s.dbuf
  idx   : 3 ≤ s.fpc → clean m s.idx
  ibuf  : 3 ≤ s.fpc → s.ipin = s.igen → clean m s.ibuf
  git   : 4 ≤ s.fpc → clean m s.git

theorem inv_step (m : Nat → Bool) (s : St) (e : Ev) (h : Inv m s) : Inv m (step true m s e) := by
  cases e with
  | say w =>
    refine ⟨h.dle, h.ile, h.fle, ?_, h.recs, h.dbuf, h.idx, h.ibuf, h.git⟩
    intro hf
    have hd : decide (1 ≤ s.fpc) = true := decide_eq_true hf
    show clean m (s.tx ++ [(w, decide (1 ≤ s.fpc))])
    rw [hd]
    exact clean_append (h.tx hf) (clean_new m w)
  | fRedact =>
    simp only [step]
    split
    · rename_i h0
      refine ⟨h.dle, h.ile, by simp, fun _ => clean_scrub m s.tx, ?_, ?_, ?_, ?_, ?_⟩ <;>
        (intro hf; simp at hf)
    · exact h
  | fRecords =>
    simp only [step]
    split
    · rename_i h1
      refine ⟨Nat.le_succ_of_le h.dle, h.ile, by simp, fun _ => h.tx (by omega),
        fun _ => clean_scrub m s.recs, ?_, ?_, ?_, ?_⟩
      · intro _ hp
        have := h.dle
        simp at hp
        omega
      · intro hf; simp at hf
      · intro hf; simp at hf
      · intro hf; simp at hf
    · exact h
  | fIndex =>
    simp only [step]
    split
    · rename_i h2
      refine ⟨h.dle, Nat.le_succ_of_le h.ile, by simp, fun _ => h.tx (by omega),
        fun _ => h.recs (by omega), fun _ hp => h.dbuf (by omega) hp,
        fun _ => clean_scrub m s.idx, ?_, ?_⟩
      · intro _ hp
        have := h.ile
        simp at hp
        omega
      · intro hf; simp at hf
    · exact h
  | fSquash =>
    simp only [step]
    split
    · rename_i h3
      exact ⟨h.dle, h.ile, by simp, fun _ => h.tx (by omega), fun _ => h.recs (by omega),
        fun _ hp => h.dbuf (by omega) hp, fun _ => h.idx (by omega), fun _ hp => h.ibuf (by omega) hp,
        fun _ => h.recs (by omega)⟩
    · exact h
  | dPin =>
    exact ⟨Nat.le_refl _, h.ile, h.fle, h.tx, h.recs, fun _ _ => clean_nil m, h.idx, h.ibuf, h.git⟩
  | dReadDay =>
    exact ⟨h.dle, h.ile, h.fle, h.tx, h.recs,
      fun (hf : 2 ≤ s.fpc) hp => clean_append (h.dbuf hf hp) (h.tx (by omega)), h.idx, h.ibuf, h.git⟩
  | dReadRecs =>
    exact ⟨h.dle, h.ile, h.fle, h.tx, h.recs,
      fun hf hp => clean_append (h.dbuf hf hp) (h.recs hf), h.idx, h.ibuf, h.git⟩
  | dWrite =>
    simp only [step]
    split
    · rename_i hg
      have hp : s.gen = s.dpin := by simpa using hg
      exact ⟨h.dle, h.ile, h.fle, h.tx,
        fun hf => clean_append (h.recs hf) (h.dbuf hf hp.symm), h.dbuf, h.idx, h.ibuf,
        fun (hf : 4 ≤ s.fpc) => clean_append (h.git hf) (h.dbuf (by omega) hp.symm)⟩
    · exact h
  | iPin =>
    exact ⟨h.dle, Nat.le_refl _, h.fle, h.tx, h.recs, h.dbuf, h.idx, fun _ _ => clean_nil m, h.git⟩
  | iRead =>
    exact ⟨h.dle, h.ile, h.fle, h.tx, h.recs, h.dbuf, h.idx,
      fun (hf : 3 ≤ s.fpc) hp => clean_append (h.ibuf hf hp) (h.tx (by omega)), h.git⟩
  | iAdd =>
    simp only [step]
    split
    · rename_i hg
      have hp : s.igen = s.ipin := by simpa using hg
      exact ⟨h.dle, h.ile, h.fle, h.tx, h.recs, h.dbuf,
        fun hf => clean_append (h.idx hf) (h.ibuf hf hp.symm), h.ibuf, h.git⟩
    · exact h

theorem inv_run (m : Nat → Bool) (es : List Ev) : ∀ s, Inv m s → Inv m (run true m s es) := by
  induction es with
  | nil => intro s h; exact h
  | cons e es ih => intro s h; exact ih (step true m s e) (inv_step m s e h)

theorem inv_init (m : Nat → Bool) (s : St) (h0 : s.fpc = 0) (hd : s.dpin ≤ s.gen) (hi : s.ipin ≤ s.igen) :
    Inv m s := by
  refine ⟨hd, hi, by omega, ?_, ?_, ?_, ?_, ?_, ?_⟩ <;> intros <;> omega

/-- For every forgotten predicate, every starting state (any transcripts, records, history and index;
no forget begun; no pin ahead of its generation) and every interleaving of the forget with dream
reads/writes, index reads/adds and new speech: once the forget has returned ok, the records, their
git history and the index hold no forgotten line — and every later state too (any extension of the
event list is again an event list). -/
theorem fixed_invariant (m : Nat → Bool) (s0 : St) (es : List Ev)
    (h0 : s0.fpc = 0) (hd : s0.dpin ≤ s0.gen) (hi : s0.ipin ≤ s0.igen) :
    (run true m s0 es).fpc = 4 →
      clean m (run true m s0 es).recs ∧ clean m (run true m s0 es).git ∧
      clean m (run true m s0 es).idx := by
  intro hf
  have h := inv_run m es s0 (inv_init m s0 h0 hd hi)
  exact ⟨h.recs (by omega), h.git (by omega), h.idx (by omega)⟩

end Eve.Forget
