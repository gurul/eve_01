/-
  The link and the note in a watch alert (bridge/src/eve_bridge/watch.py `model_reading`, `evaluate`;
  telegram.py `tell_owner` -> `_say` -> telegram_format.compose). Line numbers are those of watch.py as reviewed
  (1792 lines, before the fix). An alert is texted as eve's own message and goes into the chat's history as
  eve's words (`tell_owner`, `_note("eve", ...)`). The property:

    an alert for a page watch links only the page the owner gave; an alert for a search watch links only a
    link the search reading gave; and no alert carries a link hidden behind other words.

  How the code builds it. `model_reading` (:868-883) keeps a `url` from ANY model answer that starts with
  http(s) (:880-881), for the page-text reader and the screenshot reader too, whose prompts (PAGE_SYSTEM,
  VISION_SYSTEM) never ask for one; those readers see the page's own words and pixels. `evaluate` (:984-1059)
  stores it (`w.last_url`, :993-994) and links `w.last_url or w.target` (:1053). The note (whitespace
  collapsed, 200 characters, :879) is added for a search or ticketmaster watch or a page watched by search
  (:1054-1055), and telegram_format turns `[words](https://...)` into a hyperlink showing only the words.

  Abstraction. A watch is a `page` or a `search` watch. A reading comes from a `reader`: `structured` (the page's
  own markup; `structured()` never sets a url), `text` (the page-text model), `vision` (the screenshot model) or
  `search` (the web-search model). `read r u n f` is one reading from reader `r`; `u` says the answer holds a
  url, `n` that its note holds a markdown link, `f` that the reading makes the condition fire. The stored link is
  `none`, from the `search` reader, or from a `pageModel` (text or vision). The owner's own page is the target,
  linked when nothing is stored.

  The fix: `model_reading` keeps a url only for `source == "search"` and turns `[`/`]` in the note into `(`/`)`;
  `evaluate` links `w.target` for a page watch. It passes the Python replays. Bound: none. The step lemma is
  checked by `decide` on all 2 x 3 x 2 x 2 = 24 states and all 32 events; `fixed_invariant` then holds for every
  trace of any length, by induction.

  As implemented (re-verification, 2026-09-25): the note is made plain in `evaluate` (`plain_note`) for every
  kind that shows one, Ticketmaster included, whose event and presale names are Ticketmaster's words; this model's
  Kind has page and search only. A search's url is kept only when `_plain_link` finds nothing the formatter could
  read as markup (no brackets, parentheses, quotes, angle brackets, spaces), since a url itself can carry
  `[words](link)`.
-/
namespace Eve.WatchLink

inductive Kind where
  | page
  | search
  deriving DecidableEq, Repr

inductive Reader where
  | structured
  | text
  | vision
  | search
  deriving DecidableEq, Repr

inductive Stored where
  | none
  | search
  | pageModel
  deriving DecidableEq, Repr

inductive Event where
  | read (r : Reader) (url note fires : Bool)
  deriving DecidableEq, Repr

structure S where
  kind : Kind
  stored : Stored := .none
  badLink : Bool := false     -- ghost: an alert linked what the property does not allow
  hidden : Bool := false      -- ghost: an alert carried a link hidden behind words
  deriving DecidableEq, Repr

def Spec (s : S) : Bool := !s.badLink && !s.hidden

def fromReader : Reader → Stored
  | .search => .search
  | _ => .pageModel

/-- What the alert links: `.none` stands for the owner's own target on a page watch, or no link on a search
watch. -/
def allowed (k : Kind) (linked : Stored) : Bool :=
  match k, linked with
  | .page, .none => true
  | .search, .none => true
  | .search, .search => true
  | _, _ => false

/-- The note goes into the alert (:1054): a search watch, or a page watch whose reading came from search. -/
def noteShown (k : Kind) (r : Reader) : Bool := k == .search || r == .search

/-! ## The current code -/

def Cur.step (s : S) : Event → S
  | .read r u n f =>
    -- model_reading keeps any http(s) url (:880-881); structured() never sets one
    let stored := if u && r != .structured then fromReader r else s.stored
    if f then
      { s with stored := stored,
               badLink := s.badLink || !allowed s.kind stored,             -- :1053, w.last_url or w.target
               hidden := s.hidden || (n && r != .structured && noteShown s.kind r) }   -- :1054-1055
    else { s with stored := stored }

def Cur.run (k : Kind) (evs : List Event) : S := evs.foldl Cur.step { kind := k }

/-- A page watch: the page-text model, talked into a url by the page's words, then a firing reading. The
alert links the attacker's address, not the owner's page (`test_a_page_alert_links_only_the_page_the_owner_gave`). -/
theorem current_violates :
    Spec (Cur.run .page [.read .text true false false, .read .text false false true]) = false := by
  decide

/-- A search watch whose note holds `[claim your code](https://...)`: a hidden link in eve's own message
(`test_a_search_note_cannot_hide_a_link_behind_words`). -/
theorem current_violates_hidden_link :
    Spec (Cur.run .search [.read .search true true true]) = false := by
  decide

/-! ## The fixed code -/

def Fix.step (s : S) : Event → S
  -- the fix breaks the markdown link syntax in the note, so a note's link (the third field) never survives
  | .read r u _ f =>
    -- the fix keeps a url only from the search reader
    let stored := if u && r == .search then Stored.search else s.stored
    -- the fix links w.target for a page watch
    let linked := if s.kind == .page then Stored.none else stored
    if f then { s with stored := stored, badLink := s.badLink || !allowed s.kind linked }
    else { s with stored := stored }

def Fix.run (k : Kind) (evs : List Event) : S := evs.foldl Fix.step { kind := k }

/-- The strengthened invariant: only a search reader's url is ever stored. -/
def Inv (s : S) : Bool := Spec s && (s.stored != .pageModel)

theorem inv_step : ∀ (s : S) (e : Event), Inv s = true → Inv (Fix.step s e) = true := by
  intro s e
  rcases s with ⟨k, st, b, h⟩
  rcases e with ⟨r, u, n, f⟩
  cases k <;> cases st <;> cases b <;> cases h <;> cases r <;> cases u <;> cases n <;> cases f <;> decide

theorem inv_run (evs : List Event) : ∀ s : S, Inv s = true → Inv (evs.foldl Fix.step s) = true := by
  induction evs with
  | nil => intro s h; exact h
  | cons e rest ih => intro s h; exact ih _ (inv_step s e h)

/-- For both kinds and every trace of readings from every reader, whatever urls and notes the models answer:
a page alert links only the owner's page, a search alert only a search link, and no link is hidden. -/
theorem fixed_invariant (k : Kind) (evs : List Event) : Spec (Fix.run k evs) = true := by
  have h := inv_run evs { kind := k } (by cases k <;> decide)
  unfold Fix.run
  unfold Inv at h
  simp only [Bool.and_eq_true] at h
  exact h.1

end Eve.WatchLink
