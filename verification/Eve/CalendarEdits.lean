/-
  Eve.CalendarEdits — the calendar's writes, its merged reading, its free slots and its Maps links.

  Code modelled: bridge/src/eve_bridge/google_rest.py at a35b043 (origin/main), line numbers of that commit:
  - `_when` (228-238), `_event_body` (586-600), `_calendar_create_event` (602-611), `_calendar_update_event`
    (617-627): `bodyField`, `step`, `run`. Google's PATCH merge (a key left out keeps its value, a null clears it,
    a value sets it) is `merge` / `WhenPatch.apply`; a create starts from nothing, so the key create pops (607-608)
    is the key a null would clear.
  - `_calendar_respond_to_event` (629-639): `respond`, `respondCall`. A PATCH replaces an array whole, so the
    list sent is the event's new attendee list.
  - `_send_updates` (241-243), with the guest reads of create (609), update (620-627), respond (639) and delete
    (641-645): `sendUpdates`, `createEmails`, `updateCall`, `respondCall`, `deleteCall`.
  - `_calendar_list_events` (501-542) with `_list_one_calendar` (544-559), `_date_only` (268-271) and `_start_key`
    (341-361): `listEvents`, `single`, `merged`. The calendar list step is 508-512; the primary-first stable sort
    (515) is `ordered`; the CALENDARS_MAX cap (518-519) is `cap`; a calendar that raises GoogleError or
    httpx.HTTPError (522-525) has `answer = none`; the raise when every calendar read failed (532-533); the merge,
    the stable sort by `_start_key` and the page (534-541) with `more`.
  - `free_slots` (246-265), as `_calendar_suggest_time` (570-584) calls it: `scan` (the cursor loop, 257-262),
    `daySlots` (+ the last stretch, 263-264), `freeLoop` (the day loop and its 50-slot stop, 252), `freeSlots`
    (`sorted(busy)`, 248). Each day's window (253-254) is an input: `curWindow` is a35b043's, `ownerWindow` the fix.
  - `is_place` (294-297), `place_links` (300-305), `compact_event` (316-338): `isPlace`, `placeLinks`,
    `compactEvent`. The whitespace collapse, `_URLISH`, `_NOT_A_PLACE` and the URL quoting are left abstract
    (`Places`): P6 is proved for every such function, given only that collapsing "" gives "".

  Properties, over arbitrary inputs and traces:
  - (P1) after any sequence of creates and PATCHes, an event's start and end never hold both date and dateTime
    (`p1_run`), and a field given as all-day ends with no dateTime, a timed one with no date (`p1_switch`).
  - (P2) respond_to_event changes only the self attendee's responseStatus and sends every other attendee back
    as it was, in place; with no self attendee it sends nothing (`p2`).
  - (P3) guests are emailed (sendUpdates=all) exactly when notifyGuests says so, or, with it absent, when the
    event (as it will be after the change, or as it was for a delete) has guests (`p3`). So never with
    notifyGuests false, and with it absent only when there are guests.
  - (P4) list_events across calendars returns the soonest `page` of every event read from every calendar that
    answered, sorted by start, saying `more` when any was left out (`p4_merge`); it fails exactly when it read at
    least one calendar and every one it read failed (`P4fail`).
  - (P5) free_slots returns only stretches inside [start, end], inside one day's working hours in the owner's
    zone, at least the duration long, overlapping no busy interval (`P5`).
  - (P6) an event gets mapsUrl and directionsUrl exactly when its location is a place: not empty after
    whitespace collapse, not a link, not a call's name (`p6`).

  Findings (`current_violates`; replays in bridge/tests/test_lean_calendaredits_replay.py, all failing on
  a35b043):
  1. P4: the calendar list step (508-512) catches GoogleError only. A dropped connection there
     (httpx.HTTPError: a timeout, a reset) escaped list_events, and call_tool too, without reading any calendar,
     though the primary answered; the same failure as a GoogleError falls back to the primary, and the per-
     calendar reads (522) already catch both. Fix: `except (GoogleError, httpx.HTTPError)` at 511.
     Replay: `test_list_events_reads_the_primary_when_the_calendar_list_has_a_network_error`.
  2. P5: every day's 9-to-18 window is built in the start's fixed UTC offset (253-254). Asked across the end
     of daylight time (2026-11-01 in Los Angeles), the days after it got 08:00-17:00 local: an hour before the
     owner's working day was offered as free. Fix: build each day's window from local wall-clock hours in the
     owner's zone, resolved per day (a DST-aware tzinfo, e.g. `datetime.combine(day, time(h)).astimezone()` for
     the Mac's zone), which is `ownerWindow`.
     Replay: `test_suggest_time_keeps_to_working_hours_across_the_end_of_daylight_time`.
     The same root cause needs no clock change: the window is built in whatever offset startTime carries, and
     `_WHEN` allows any offset. Asked for 2026-10-06 in UTC ("…Z"), the slot was 09:00-18:00 UTC, which is
     02:00-11:00 in Los Angeles (`current_violates`, part P5 in UTC). The same fix covers it.
     Replay: `test_suggest_time_keeps_to_the_owners_working_hours_when_asked_in_utc`.
  Applied on 2026-10-05 (feat/apps-mcp): `except (GoogleError, httpx.HTTPError)` on the calendar list step, and
  `free_slots` takes its days from `start.astimezone().date()` and builds each bound as
  `datetime.combine(day, time(h)).astimezone()` (the Mac's zone, resolved per day: `ownerWindow`). All three
  replays pass.
  P1, P2, P3 and P6 held from the start: the fixed code is the same there, and `fixed_invariant` proves them of
  it. `p4_fail_current_else` and `p5_current_fixed_offset` show the old code kept P4 and P5 outside the two gaps.

  Where the Python goes beyond the model (stated, not proved):
  - Values are opaque Nats; `rfc3339`, `str(value)[:10]` and `_local_zone()` are the functions `Env.rfc`,
    `Env.dayOf`, `Env.localZone`. `_when` treats "" like None (`if not value`), modelled as `none`.
  - P1 rests on Google's documented PATCH semantics (null clears a key, objects merge, arrays replace). A switch
    from timed-with-timeZone to all-day keeps the old timeZone key (`_when` sends no null for it); P1 is about
    date/dateTime only. An update that gives only one of startTime/endTime can leave start timed and end all-day;
    Google refuses that event, and P1 (per field) does not speak to it.
  - P2: `rest` stands for every key of an attendee other than email/self/responseStatus (displayName, comment,
    optional, additionalGuests, …), all sent back unchanged because the GET's JSON is sent back as it is. The GET
    is made without maxAttendees, so Google does not omit attendees; on an event whose guest list is hidden from
    the owner, Google returns only what it shows them.
  - P3: notifyGuests is a JSON boolean in the schema (`_NOTIFY`), modelled as Option Bool. call_tool does not
    validate the schema, and `bool(notify)` makes a string "false" true; the model does not cover non-boolean
    values. respond_to_event's "has guests" is true because the owner is one of them (it raises otherwise).
  - P4: event identity and calendar tags are Nats; `_start_key` is `Ev.key` (an all-day date at its calendar's
    midnight, an unreadable start last); the single-calendar path returns Google's own order (orderBy=startTime)
    and is not re-sorted. Only the first 50 calendars of calendarList are seen (`_page({"pageSize": 50}, 50)`),
    and a body that is not JSON (r.json() raising ValueError, or a str for a non-JSON content type, which then
    fails on `.get`) is not a GoogleError/httpx.HTTPError, so it aborts the merge; Google sends JSON. An event on
    two calendars comes back twice. calendarsRead, unreadable and timeZone are not modelled.
  - P5: times are Int minutes, the zone a function from local wall minutes to instants (`toUtc`); the days are
    an arbitrary list (the code takes consecutive dates from start.date() while their midnight is before end).
    `hour=day_start` outside 0-23 raises ValueError (an error to the model, not a slot), and day_end above 23 is
    cut to 23. `_calendar_suggest_time` reads `workdayStartHour or 9`, so an asked start hour of 0 becomes 9 (the
    slots stay inside the asked hours, but fewer are offered); weekends are working days. freeBusy marks a
    calendar it could not read with `errors` and no busy time, and `_calendar_suggest_time` counts it as free:
    P5 is about the busy time the code was given. `free_slots` sorts busy by (start, end); `freeSlots` sorts by
    start only (stable). `scan_spec` holds for every start-sorted order, Python's among them, so P5 covers it, but
    `freeSlots` is not definitionally the Python's order for ties. P5's "at least the duration long" is
    `m ≤ s.2 - s.1`: with durationMinutes 0 the code offers zero-length slots, and with a negative one it offers
    inverted ones (e.g. 10:00-09:40 inside a busy stretch, which `Free` does not catch); P5 does not exclude them.
  - P4 (more): when every calendar read fails with httpx.HTTPError, `raise failure` re-raises it and call_tool
    (which catches GoogleError/KeyError/ValueError) lets it escape as an exception, not an isError result; the
    model counts both as `none`. Items of a calendar's answer that are not dicts are dropped by the merge path.
    Each merged event is tagged with its calendar's summary, not its id.
  - P6: what `_URLISH` counts as a link is the code's own: "zoom.us" with no path, or a shortener on a domain it
    does not list ("bit.ly/x"), is not a link to it and gets a Maps link. P6 is proved for any detector.
-/
namespace Eve.CalendarEdits

/-! ## P1 -/

inductive Val where
  | absent
  | null
  | set (v : Nat)
  deriving DecidableEq, Repr

def merge (old : Option Nat) : Val → Option Nat
  | .absent => old
  | .null => none
  | .set v => some v

structure When where
  date : Option Nat := none
  dateTime : Option Nat := none
  timeZone : Option Nat := none
  deriving DecidableEq, Repr

structure WhenPatch where
  date : Val
  dateTime : Val
  timeZone : Val
  deriving DecidableEq, Repr

def WhenPatch.apply (w : When) (p : WhenPatch) : When :=
  { date := merge w.date p.date, dateTime := merge w.dateTime p.dateTime,
    timeZone := merge w.timeZone p.timeZone }

structure Env where
  dayOf : Nat → Nat
  rfc : Nat → Nat
  localZone : Nat

def whenOf (e : Env) (value : Option Nat) (allDay : Bool) (tz : Option Nat) : Option WhenPatch :=
  match value with
  | none => none
  | some v =>
      if allDay then some ⟨.set (e.dayOf v), .null, .absent⟩
      else some ⟨.null, .set (if tz.isSome then v else e.rfc v),
                 match tz with | some z => .set z | none => .absent⟩

structure Args where
  startTime : Option Nat := none
  endTime : Option Nat := none
  allDay : Bool := false
  timeZone : Option Nat := none
  recurrence : Bool := false
  deriving DecidableEq, Repr

def recurZone (e : Env) (a : Args) (p : WhenPatch) : WhenPatch :=
  match p.dateTime with
  | .set _ => if a.recurrence && a.timeZone.isNone then { p with timeZone := .set e.localZone } else p
  | _ => p

def bodyField (e : Env) (a : Args) (value : Option Nat) : Option WhenPatch :=
  (whenOf e value a.allDay a.timeZone).map (recurZone e a)

structure Event where
  start : When := {}
  stop : When := {}
  deriving DecidableEq, Repr

def patchField (w : When) : Option WhenPatch → When
  | none => w
  | some p => p.apply w

inductive Op where
  | create (a : Args)
  | update (a : Args)

def step (e : Env) : Option Event → Op → Option Event
  | _, .create a => some { start := patchField {} (bodyField e a a.startTime),
                           stop := patchField {} (bodyField e a a.endTime) }
  | none, .update _ => none
  | some ev, .update a => some { start := patchField ev.start (bodyField e a a.startTime),
                                 stop := patchField ev.stop (bodyField e a a.endTime) }

def run (e : Env) (s : Option Event) (ops : List Op) : Option Event := ops.foldl (step e) s

def OneKey (w : When) : Prop := ¬ (w.date.isSome ∧ w.dateTime.isSome)

def EventOk : Option Event → Prop
  | none => True
  | some ev => OneKey ev.start ∧ OneKey ev.stop

theorem bodyField_oneKey (e : Env) (a : Args) (v : Option Nat) (w : When) (hw : OneKey w) :
    OneKey (patchField w (bodyField e a v)) := by
  cases v with
  | none => simpa [bodyField, whenOf, patchField] using hw
  | some x =>
      unfold OneKey
      by_cases hd : a.allDay
      · simp [bodyField, whenOf, hd, recurZone, patchField, WhenPatch.apply, merge]
      · cases htz : a.timeZone <;> cases hr : a.recurrence <;>
          simp [bodyField, whenOf, hd, htz, hr, recurZone, patchField, WhenPatch.apply, merge]

theorem p1_step (e : Env) (s : Option Event) (op : Op) (h : EventOk s) : EventOk (step e s op) := by
  cases op with
  | create a =>
      exact ⟨bodyField_oneKey e a _ {} (by simp [OneKey]), bodyField_oneKey e a _ {} (by simp [OneKey])⟩
  | update a =>
      cases s with
      | none => trivial
      | some ev => exact ⟨bodyField_oneKey e a _ _ h.1, bodyField_oneKey e a _ _ h.2⟩

theorem p1_run (e : Env) : ∀ (ops : List Op) (s : Option Event), EventOk s → EventOk (run e s ops)
  | [], _, h => h
  | op :: ops, s, h => p1_run e ops (step e s op) (p1_step e s op h)

/-- The switch itself: a field given as all-day ends with no dateTime, a timed one with no date, whatever the
event held before (Google's PATCH: a null clears the key). -/
theorem p1_switch (e : Env) (a : Args) (v : Nat) (w : When) :
    (a.allDay = true → (patchField w (bodyField e a (some v))).dateTime = none ∧
        (patchField w (bodyField e a (some v))).date = some (e.dayOf v)) ∧
    (a.allDay = false → (patchField w (bodyField e a (some v))).date = none ∧
        (patchField w (bodyField e a (some v))).dateTime.isSome) := by
  constructor
  · intro hd; simp [bodyField, whenOf, hd, recurZone, patchField, WhenPatch.apply, merge]
  · intro hd
    cases htz : a.timeZone <;> cases hr : a.recurrence <;>
      simp [bodyField, whenOf, hd, htz, hr, recurZone, patchField, WhenPatch.apply, merge]

/-! ## P2 -/

structure Guest where
  email : Nat
  self : Bool
  status : Nat
  rest : Nat
  deriving DecidableEq, Repr

def respond (gs : List Guest) (r : Nat) : Option (List Guest) :=
  if gs.any (·.self) then some (gs.map fun g => if g.self then { g with status := r } else g) else none

def P2 (gs : List Guest) (r : Nat) : Prop :=
  (respond gs r = none ↔ ∀ g ∈ gs, g.self = false) ∧
  ∀ out, respond gs r = some out →
    out.length = gs.length ∧
    ∀ i (h : i < gs.length), ∃ h' : i < out.length,
      (gs[i].self = false → out[i] = gs[i]) ∧
      (gs[i].self = true → out[i] = { gs[i] with status := r })

theorem p2 (gs : List Guest) (r : Nat) : P2 gs r := by
  refine ⟨?_, ?_⟩
  · unfold respond
    by_cases h : gs.any (·.self) <;> simp_all
  · intro out hout
    unfold respond at hout
    split at hout
    · cases hout
      refine ⟨by simp, fun i h => ⟨by simpa using h, ?_, ?_⟩⟩
      · intro hs; simp [hs]
      · intro hs; simp [hs]
    · cases hout

/-! ## P3 -/

def sendUpdates (notify : Option Bool) (hasGuests : Bool) : Bool :=
  match notify with
  | none => hasGuests
  | some b => b

def wanted (notify : Option Bool) (hasGuests : Bool) : Bool :=
  match notify with
  | none => hasGuests
  | some b => b

def createEmails (notify : Option Bool) (attendeeEmails : List Nat) : Bool :=
  sendUpdates notify (!attendeeEmails.isEmpty)

def newGuest (e : Nat) : Guest := { email := e, self := false, status := 0, rest := 0 }

def updateCall (notify : Option Bool) (add : List Nat) (ev : List Guest) : Bool × Option (List Guest) :=
  let guests := if !add.isEmpty || notify.isNone then ev else []
  let body : Option (List Guest) :=
    if !add.isEmpty then some (guests ++ (add.filter fun x => !(guests.map (·.email)).contains x).map newGuest)
    else none
  (sendUpdates notify (!guests.isEmpty || (match body with | some l => !l.isEmpty | none => false)), body)

def respondCall (notify : Option Bool) (ev : List Guest) (r : Nat) : Option (Bool × List Guest) :=
  (respond ev r).map fun out => (sendUpdates notify true, out)

def deleteCall (notify : Option Bool) (ev : List Guest) : Bool :=
  let guests := if notify.isNone then ev else []
  sendUpdates notify (!guests.isEmpty)

def after (ev : List Guest) : Option (List Guest) → List Guest
  | none => ev
  | some l => l

def P3 : Prop :=
  (∀ notify (emails : List Nat), createEmails notify emails = wanted notify (!emails.isEmpty)) ∧
  (∀ notify add ev, (updateCall notify add ev).1 = wanted notify (!(after ev (updateCall notify add ev).2).isEmpty)) ∧
  (∀ notify ev r x, respondCall notify ev r = some x → x.1 = wanted notify (!ev.isEmpty) ∧ x.2 = (respond ev r).getD []) ∧
  (∀ notify ev, deleteCall notify ev = wanted notify (!ev.isEmpty))

theorem p3 : P3 := by
  refine ⟨?_, ?_, ?_, ?_⟩
  · intro notify emails; cases notify <;> rfl
  · intro notify add ev
    cases notify with
    | some b => cases hadd : add.isEmpty <;> simp [updateCall, sendUpdates, wanted, hadd]
    | none =>
        cases hadd : add.isEmpty
        · simp [updateCall, sendUpdates, wanted, hadd, after]
          cases ev <;> simp
        · simp [updateCall, sendUpdates, wanted, hadd, after]
  · intro notify ev r x hx
    unfold respondCall at hx
    cases hr : respond ev r with
    | none => rw [hr] at hx; cases hx
    | some out =>
        rw [hr] at hx; cases hx
        have hne : ev ≠ [] := by
          intro h; subst h; simp [respond] at hr
        cases notify <;> simp [sendUpdates, wanted, hne]
  · intro notify ev; cases notify <;> simp [deleteCall, sendUpdates, wanted]

/-! ## P6 -/

structure Places where
  norm : String → String
  urlish : String → Bool
  callName : String → Bool
  enc : String → String

def isPlace (e : Places) (loc : String) : Bool :=
  e.norm loc != "" && !e.urlish (e.norm loc) && !e.callName (e.norm loc)

structure Links where
  mapsUrl : Option String
  directionsUrl : Option String
  deriving DecidableEq, Repr

def mapsSearch : String := "https://www.google.com/maps/search/?api=1&query="
def mapsDir : String := "https://www.google.com/maps/dir/?api=1&destination="

def placeLinks (e : Places) (loc : String) : Links :=
  if isPlace e loc then ⟨some (mapsSearch ++ e.enc loc), some (mapsDir ++ e.enc loc)⟩ else ⟨none, none⟩

structure GEvent where
  summary : Option String
  location : Option String
  mapsUrl : Option String
  deriving DecidableEq, Repr

structure Compact where
  summary : Option String
  location : Option String
  links : Links
  deriving DecidableEq, Repr

def compactEvent (e : Places) (ev : GEvent) : Compact :=
  { summary := ev.summary.filter (· != ""), location := ev.location.filter (· != ""),
    links := placeLinks e (ev.location.getD "") }

def P6 (e : Places) (ev : GEvent) : Prop :=
  ((compactEvent e ev).links.mapsUrl.isSome ↔ ∃ l, ev.location = some l ∧ isPlace e l = true) ∧
  ((compactEvent e ev).links.directionsUrl.isSome ↔ ∃ l, ev.location = some l ∧ isPlace e l = true) ∧
  (∀ l, ev.location = some l → (e.norm l = "" ∨ e.urlish (e.norm l) = true) →
     (compactEvent e ev).links = ⟨none, none⟩)

theorem p6 (e : Places) (hempty : e.norm "" = "") (ev : GEvent) : P6 e ev := by
  have key : isPlace e (ev.location.getD "") = true ↔ ∃ l, ev.location = some l ∧ isPlace e l = true := by
    cases hl : ev.location with
    | none => simp [isPlace, hempty]
    | some l => simp
  refine ⟨?_, ?_, ?_⟩
  · rw [← key]; unfold compactEvent placeLinks; split <;> simp_all
  · rw [← key]; unfold compactEvent placeLinks; split <;> simp_all
  · intro l hl hbad
    have : isPlace e l = false := by
      rcases hbad with h | h <;> simp [isPlace, h]
    simp [compactEvent, placeLinks, hl, this]

/-! ## P4 -/

structure Ev where
  id : Nat
  key : Int
  deriving DecidableEq, Repr

structure Tagged where
  cal : Nat
  ev : Ev
  deriving DecidableEq, Repr

def byStart (a b : Tagged) : Bool := decide (a.ev.key ≤ b.ev.key)

structure Cal where
  id : Nat
  primary : Bool
  skip : Bool
  answer : Option (List Ev × Bool)
  deriving DecidableEq, Repr

inductive Listing where
  | ok (cals : List Cal)
  | googleErr
  | netErr
  deriving DecidableEq, Repr

structure Out where
  events : List Tagged
  more : Bool
  deriving DecidableEq, Repr

def primaryId : Nat := 0

def single (p : Option (List Ev × Bool)) : Option Out :=
  p.map fun x => ⟨x.1.map (Tagged.mk primaryId), x.2⟩

def ordered (cals : List Cal) : List Cal := cals.filter (·.primary) ++ cals.filter (!·.primary)

def readSet (cap : Nat) (cals : List Cal) : List Cal := (ordered cals).take cap

def answered (cap : Nat) (cals : List Cal) : List Tagged :=
  ((readSet cap cals).map fun c => match c.answer with
    | some x => x.1.map (Tagged.mk c.id)
    | none => []).flatten

def merged (cap page : Nat) (cals : List Cal) : Option Out :=
  let read := readSet cap cals
  if !read.isEmpty && read.all (·.answer.isNone) then none
  else
    let all := answered cap cals
    some ⟨(all.mergeSort byStart).take page,
          decide (cap < cals.length) || read.any (fun c => match c.answer with | some x => x.2 | none => false)
            || decide (page < all.length)⟩

def listEvents (fixNet : Bool) (cap page : Nat) (l : Listing) (p : Option (List Ev × Bool)) : Option Out :=
  match l with
  | .netErr => if fixNet then single p else none
  | .googleErr => single p
  | .ok cs =>
      let cals := cs.filter (!·.skip)
      if cals.isEmpty then single p else merged cap page cals

def tried (fixNet : Bool) (cap : Nat) (l : Listing) (p : Option (List Ev × Bool)) : List (Option (List Ev × Bool)) :=
  match l with
  | .netErr => if fixNet then [p] else []
  | .googleErr => [p]
  | .ok cs =>
      let cals := cs.filter (!·.skip)
      if cals.isEmpty then [p] else (readSet cap cals).map (·.answer)

/-- P4's failure half: the call fails exactly when it read at least one calendar and every one it read failed. -/
def P4fail (fixNet : Bool) (cap page : Nat) (l : Listing) (p : Option (List Ev × Bool)) : Prop :=
  listEvents fixNet cap page l p = none ↔
    (tried fixNet cap l p ≠ [] ∧ ∀ a ∈ tried fixNet cap l p, a = none)

/-- P4's merge half: what comes back is the soonest `page` of every event read from every calendar that
answered, sorted by start; when any was left out, `more` says so. -/
def P4merge (cap page : Nat) (cals : List Cal) : Prop :=
  ∀ o, merged cap page cals = some o →
    o.events.Pairwise (fun a b => byStart a b = true) ∧
    (∀ x ∈ o.events, x ∈ answered cap cals) ∧
    o.events = ((answered cap cals).mergeSort byStart).take page ∧
    ((answered cap cals).length ≤ page → ∀ x ∈ answered cap cals, x ∈ o.events) ∧
    (page < (answered cap cals).length → o.more = true)

theorem mem_answered (cap : Nat) (cals : List Cal) (x : Tagged) :
    x ∈ answered cap cals ↔ ∃ c ∈ readSet cap cals, ∃ y, c.answer = some y ∧ x.ev ∈ y.1 ∧ x.cal = c.id := by
  unfold answered
  simp only [List.mem_flatten, List.mem_map]
  constructor
  · rintro ⟨l, ⟨c, hc, rfl⟩, hx⟩
    cases ha : c.answer with
    | none => simp [ha] at hx
    | some y =>
        simp only [ha, List.mem_map] at hx
        obtain ⟨e, he, rfl⟩ := hx
        exact ⟨c, hc, y, ha, he, rfl⟩
  · rintro ⟨c, hc, y, ha, hx, hcal⟩
    refine ⟨_, ⟨c, hc, rfl⟩, ?_⟩
    simp only [ha, List.mem_map]
    exact ⟨x.ev, hx, by cases x; simp_all⟩

theorem byStart_trans (a b c : Tagged) : byStart a b = true → byStart b c = true → byStart a c = true := by
  simp only [byStart, decide_eq_true_eq]; omega

theorem byStart_total (a b : Tagged) : (byStart a b || byStart b a) = true := by
  simp only [byStart, Bool.or_eq_true, decide_eq_true_eq]; omega

theorem p4_merge (cap page : Nat) (cals : List Cal) : P4merge cap page cals := by
  intro o ho
  unfold merged at ho
  dsimp only at ho
  split at ho
  · cases ho
  · cases ho
    have hs := List.pairwise_mergeSort byStart_trans byStart_total (answered cap cals)
    refine ⟨hs.sublist (List.take_sublist _ _), ?_, rfl, ?_, ?_⟩
    · intro x hx
      exact List.mem_mergeSort.mp (List.mem_of_mem_take hx)
    · intro hle x hx
      have hlen : ((answered cap cals).mergeSort byStart).length ≤ page := by simpa using hle
      simp only [List.take_of_length_le hlen]
      exact List.mem_mergeSort.mpr hx
    · intro hlt; simp [hlt]

theorem single_none (p : Option (List Ev × Bool)) : single p = none ↔ p = none := by
  cases p <;> simp [single]

theorem p4_fail_fixed (cap page : Nat) (hcap : 0 < cap) (l : Listing) (p : Option (List Ev × Bool)) :
    P4fail true cap page l p := by
  unfold P4fail listEvents tried
  cases l with
  | netErr => simp [single_none]
  | googleErr => simp [single_none]
  | ok cs =>
      simp only
      split
      · simp [single_none]
      · rename_i hne
        have hread : readSet cap (cs.filter (!·.skip)) ≠ [] := by
          unfold readSet ordered
          intro h
          rw [List.take_eq_nil_iff] at h
          rcases h with h | h
          · omega
          · apply hne
            have hl := congrArg List.length h
            simp only [List.length_append, List.length_nil] at hl
            have h1 : (List.filter (fun x => x.primary) (cs.filter (!·.skip))).length +
                (List.filter (fun x => !x.primary) (cs.filter (!·.skip))).length =
                (cs.filter (!·.skip)).length := by
              rw [← List.length_append]
              exact (List.filter_append_perm _ _).length_eq
            simp [List.isEmpty_iff, ← List.length_eq_zero_iff]; omega
        unfold merged
        dsimp only
        split
        · rename_i h
          simp only [Bool.and_eq_true, Bool.not_eq_true', List.isEmpty_eq_false_iff, List.all_eq_true,
            Option.isNone_iff_eq_none] at h
          simp only [ne_eq, List.map_eq_nil_iff, hread, not_false_eq_true, List.mem_map, forall_exists_index,
            and_imp, forall_apply_eq_imp_iff₂, true_and, true_iff]
          exact h.2
        · rename_i h
          simp only [Bool.and_eq_true, Bool.not_eq_true', List.isEmpty_eq_false_iff, List.all_eq_true,
            Option.isNone_iff_eq_none, not_and] at h
          simp only [reduceCtorEq, false_iff, not_and, List.mem_map, forall_exists_index,
            and_imp, forall_apply_eq_imp_iff₂]
          intro _; exact h hread

/-! ## P5 -/

abbrev Span := Int × Int

def Free (s b : Span) : Prop := s.2 ≤ b.1 ∨ b.2 ≤ s.1

def scan (m hi : Int) : Int → List Span → List Span × Int
  | c, [] => ([], c)
  | c, b :: bs =>
      if b.2 ≤ c ∨ hi ≤ b.1 then scan m hi c bs
      else
        let r := scan m hi (max c b.2) bs
        ((if m ≤ b.1 - c then [(c, b.1)] else []) ++ r.1, r.2)

def daySlots (m : Int) (busy : List Span) (lo hi : Int) : List Span :=
  let r := scan m hi lo busy
  r.1 ++ (if r.2 < hi ∧ m ≤ hi - r.2 then [(r.2, hi)] else [])

def freeLoop (m : Int) (busy : List Span) (start stop : Int) : List Span → List Span → List Span
  | out, [] => out
  | out, w :: ws =>
      if out.length < 50 then freeLoop m busy start stop (out ++ daySlots m busy (max start w.1) (min stop w.2)) ws
      else out

def byFirst (a b : Span) : Bool := decide (a.1 ≤ b.1)

def freeSlots (m : Int) (busy : List Span) (start stop : Int) (wins : List Span) : List Span :=
  freeLoop m (busy.mergeSort byFirst) start stop [] wins

theorem scan_spec (m hi : Int) : ∀ (bs : List Span) (c : Int), bs.Pairwise (fun a b => a.1 ≤ b.1) →
    c ≤ (scan m hi c bs).2 ∧ (∀ b ∈ bs, b.2 ≤ (scan m hi c bs).2 ∨ hi ≤ b.1) ∧
    ∀ s ∈ (scan m hi c bs).1, c ≤ s.1 ∧ s.2 ≤ hi ∧ m ≤ s.2 - s.1 ∧ ∀ b ∈ bs, Free s b
  | [], c, _ => by simp [scan]
  | b :: bs, c, hp => by
      have hp' := List.pairwise_cons.mp hp
      unfold scan
      split
      · rename_i hskip
        obtain ⟨ih1, ih2, ih3⟩ := scan_spec m hi bs c hp'.2
        refine ⟨ih1, ?_, ?_⟩
        · intro b' hb'
          rcases List.mem_cons.mp hb' with rfl | hb'
          · rcases hskip with h | h
            · left; omega
            · right; exact h
          · exact ih2 b' hb'
        · intro s hs
          obtain ⟨a1, a2, a3, a4⟩ := ih3 s hs
          refine ⟨a1, a2, a3, ?_⟩
          intro b' hb'
          rcases List.mem_cons.mp hb' with rfl | hb'
          · unfold Free
            rcases hskip with h | h
            · right; omega
            · left; omega
          · exact a4 b' hb'
      · rename_i hproc
        have hn : ¬ b.2 ≤ c ∧ ¬ hi ≤ b.1 := not_or.mp hproc
        obtain ⟨ih1, ih2, ih3⟩ := scan_spec m hi bs (max c b.2) hp'.2
        refine ⟨by simp only; omega, ?_, ?_⟩
        · intro b' hb'
          rcases List.mem_cons.mp hb' with rfl | hb'
          · left; simp only; omega
          · exact ih2 b' hb'
        · intro s hs
          simp only [List.mem_append] at hs
          rcases hs with hs | hs
          · split at hs
            · rename_i hm
              simp only [List.mem_singleton] at hs
              subst hs
              refine ⟨Int.le_refl _, by simp only; omega, hm, ?_⟩
              intro b' hb'
              unfold Free
              rcases List.mem_cons.mp hb' with rfl | hb'
              · left; exact Int.le_refl _
              · left; exact hp'.1 b' hb'
            · simp at hs
          · obtain ⟨a1, a2, a3, a4⟩ := ih3 s hs
            refine ⟨by omega, a2, a3, ?_⟩
            intro b' hb'
            rcases List.mem_cons.mp hb' with rfl | hb'
            · unfold Free; right; omega
            · exact a4 b' hb'

theorem daySlots_spec (m : Int) (busy : List Span) (lo hi : Int) (hp : busy.Pairwise (fun a b => a.1 ≤ b.1)) :
    ∀ s ∈ daySlots m busy lo hi, lo ≤ s.1 ∧ s.2 ≤ hi ∧ m ≤ s.2 - s.1 ∧ ∀ b ∈ busy, Free s b := by
  obtain ⟨h1, h2, h3⟩ := scan_spec m hi busy lo hp
  intro s hs
  unfold daySlots at hs
  simp only [List.mem_append] at hs
  rcases hs with hs | hs
  · exact h3 s hs
  · split at hs
    · rename_i hc
      simp only [List.mem_singleton] at hs
      subst hs
      refine ⟨h1, Int.le_refl _, hc.2, ?_⟩
      intro b hb
      unfold Free
      rcases h2 b hb with h | h
      · right; exact h
      · left; exact h
    · simp at hs

/-- Every slot: inside [start, stop], inside one of the day windows, at least `m` long, free of every busy span. -/
def SlotOk (m : Int) (busy : List Span) (start stop : Int) (wins : List Span) (s : Span) : Prop :=
  start ≤ s.1 ∧ s.2 ≤ stop ∧ m ≤ s.2 - s.1 ∧ (∃ w ∈ wins, w.1 ≤ s.1 ∧ s.2 ≤ w.2) ∧ ∀ b ∈ busy, Free s b

theorem freeLoop_spec (m : Int) (busy sorted : List Span) (hperm : ∀ b, b ∈ sorted ↔ b ∈ busy)
    (hp : sorted.Pairwise (fun a b => a.1 ≤ b.1)) (start stop : Int) (all : List Span) :
    ∀ (ws out : List Span), (∀ w ∈ ws, w ∈ all) → (∀ s ∈ out, SlotOk m busy start stop all s) →
      ∀ s ∈ freeLoop m sorted start stop out ws, SlotOk m busy start stop all s
  | [], out, _, hout => by simpa [freeLoop] using hout
  | w :: ws, out, hws, hout => by
      unfold freeLoop
      split
      · apply freeLoop_spec m busy sorted hperm hp start stop all ws
        · intro w' hw'; exact hws w' (List.mem_cons_of_mem _ hw')
        · intro s hs
          simp only [List.mem_append] at hs
          rcases hs with hs | hs
          · exact hout s hs
          · obtain ⟨a1, a2, a3, a4⟩ := daySlots_spec m sorted _ _ hp s hs
            refine ⟨by omega, by omega, a3, ⟨w, hws w List.mem_cons_self, by omega, by omega⟩, ?_⟩
            intro b hb; exact a4 b ((hperm b).mpr hb)
      · exact hout

theorem freeSlots_spec (m : Int) (busy : List Span) (start stop : Int) (wins : List Span) :
    ∀ s ∈ freeSlots m busy start stop wins, SlotOk m busy start stop wins s := by
  have hp := List.pairwise_mergeSort (le := byFirst)
    (by intro a b c; simp only [byFirst, decide_eq_true_eq]; omega)
    (by intro a b; simp only [byFirst, Bool.or_eq_true, decide_eq_true_eq]; omega) busy
  have hp' : (busy.mergeSort byFirst).Pairwise (fun a b => a.1 ≤ b.1) := by
    simpa [byFirst] using hp
  exact freeLoop_spec m busy _ (fun b => List.mem_mergeSort) hp' start stop wins wins [] (fun w h => h)
    (by simp)

/-- A zone: `toUtc w` is the instant (minutes) of local wall time `w` (minutes since day 0's midnight), the way a
ZoneInfo resolves it. A day's working window is its `ds`:00 to `min de 23`:00, local. -/
def ownerWindow (toUtc : Int → Int) (ds de : Int) (d : Int) : Span :=
  (toUtc (d * 1440 + ds * 60), toUtc (d * 1440 + (min de 23) * 60))

/-- a35b043: each day's window in the start's fixed UTC offset `off0` (instant = wall - off0). -/
def curWindow (off0 : Int) (ds de : Int) (d : Int) : Span :=
  (d * 1440 + ds * 60 - off0, d * 1440 + (min de 23) * 60 - off0)

def suggestCur (off0 ds de m : Int) (busy : List Span) (start stop : Int) (days : List Int) : List Span :=
  freeSlots m busy start stop (days.map (curWindow off0 ds de))

def suggestFix (toUtc : Int → Int) (ds de m : Int) (busy : List Span) (start stop : Int) (days : List Int) :
    List Span :=
  freeSlots m busy start stop (days.map (ownerWindow toUtc ds de))

/-- P5: every slot offered is inside [start, stop], inside the owner's working hours of one of the days (in the
owner's zone), at least the duration long, and overlaps no busy span. -/
def P5 (toUtc : Int → Int) (ds de m : Int) (busy : List Span) (start stop : Int) (days : List Int)
    (slots : List Span) : Prop :=
  ∀ s ∈ slots, start ≤ s.1 ∧ s.2 ≤ stop ∧ m ≤ s.2 - s.1 ∧
    (∃ d ∈ days, (ownerWindow toUtc ds de d).1 ≤ s.1 ∧ s.2 ≤ (ownerWindow toUtc ds de d).2) ∧
    ∀ b ∈ busy, Free s b

theorem p5_fixed (toUtc : Int → Int) (ds de m : Int) (busy : List Span) (start stop : Int) (days : List Int) :
    P5 toUtc ds de m busy start stop days (suggestFix toUtc ds de m busy start stop days) := by
  intro s hs
  obtain ⟨a1, a2, a3, ⟨w, hw, h1, h2⟩, a5⟩ := freeSlots_spec m busy start stop _ s hs
  obtain ⟨d, hd, rfl⟩ := List.mem_map.mp hw
  exact ⟨a1, a2, a3, ⟨d, hd, h1, h2⟩, a5⟩

/-- When the owner's zone is the fixed offset startTime carried (`toUtc w = w - off0`, no clock change and the
same offset), the code as it was keeps P5. Either difference breaks it: a clock change, or a startTime given in
another offset (`current_violates`, both P5 parts). -/
theorem p5_current_fixed_offset (off0 ds de m : Int) (busy : List Span) (start stop : Int) (days : List Int) :
    P5 (fun w => w - off0) ds de m busy start stop days (suggestCur off0 ds de m busy start stop days) := by
  intro s hs
  obtain ⟨a1, a2, a3, ⟨w, hw, h1, h2⟩, a5⟩ := freeSlots_spec m busy start stop _ s hs
  obtain ⟨d, hd, rfl⟩ := List.mem_map.mp hw
  exact ⟨a1, a2, a3, ⟨d, hd, h1, h2⟩, a5⟩

/-- At a35b043 P4's failure half held whenever the calendar list itself answered or failed with a GoogleError:
the gap is the dropped connection alone. -/
theorem p4_fail_current_else (cap page : Nat) (hcap : 0 < cap) (l : Listing) (hl : l ≠ .netErr)
    (p : Option (List Ev × Bool)) : P4fail false cap page l p := by
  have h := p4_fail_fixed cap page hcap l p
  cases l with
  | netErr => exact absurd rfl hl
  | googleErr => exact h
  | ok cs => exact h

/-! ## The counterexamples and the fixed code -/

/-- Los Angeles around 2026-11-01: wall time before 02:00 on day 1 is PDT (UTC-7), after it PST (UTC-8). -/
def laZone (w : Int) : Int := if w < 1440 + 120 then w + 420 else w + 480

theorem current_violates :
    -- (P4) the calendar list's request drops (httpx error), the primary calendar answers, the call fails
    (listEvents false 20 50 .netErr (some ([⟨1, 900⟩], false)) = none ∧
      ¬ P4fail false 20 50 .netErr (some ([⟨1, 900⟩], false))) ∧
    -- (P5) 2026-10-30 00:00 PDT to 2026-11-01 24:00 local, no busy time, an hour: the second day's window is
    -- 09:00-18:00 in UTC-7, which is 08:00-17:00 PST, an hour before the owner's working day
    (suggestCur (-420) 9 18 60 [] 420 3360 [0, 1] = [(960, 1500), (2400, 2940)] ∧
      ¬ P5 laZone 9 18 60 [] 420 3360 [0, 1] (suggestCur (-420) 9 18 60 [] 420 3360 [0, 1])) ∧
    -- (P5, in UTC) the owner is in PDT all day (no clock change); asked for 2026-10-06 00:00Z to 24:00Z, the
    -- window is 09:00-18:00 UTC, which is 02:00-11:00 PDT, in no working day of the owner's
    (suggestCur 0 9 18 60 [] 0 1440 [0] = [(540, 1080)] ∧
      ¬ P5 (fun w => w + 420) 9 18 60 [] 0 1440 [0] (suggestCur 0 9 18 60 [] 0 1440 [0])) := by
  refine ⟨⟨rfl, ?_⟩, ⟨by decide +kernel, ?_⟩, ⟨by decide +kernel, ?_⟩⟩
  · simp [P4fail, listEvents, tried]
  · intro h
    have hs := h (2400, 2940) (by decide +kernel)
    obtain ⟨_, _, _, ⟨d, hd, h1, h2⟩, _⟩ := hs
    simp only [List.mem_cons, List.not_mem_nil, or_false] at hd
    rcases hd with rfl | rfl <;> revert h1 h2 <;> decide
  · intro h
    have hs := h (540, 1080) (by decide +kernel)
    obtain ⟨_, _, _, ⟨d, hd, h1, h2⟩, _⟩ := hs
    simp only [List.mem_cons, List.not_mem_nil, or_false] at hd
    subst hd
    revert h1 h2; decide

/-- The fixed code: the dropped list falls back to the primary (as a GoogleError does), and each day's window is
the owner's working hours in the owner's zone. P1, P2, P3 and P6 are the code as it is. -/
theorem fixed_invariant :
    (∀ (e : Env) (ops : List Op) (s : Option Event), EventOk s → EventOk (run e s ops)) ∧
    (∀ (e : Env) (a : Args) (v : Nat) (w : When),
      (a.allDay = true → (patchField w (bodyField e a (some v))).dateTime = none) ∧
      (a.allDay = false → (patchField w (bodyField e a (some v))).date = none)) ∧
    (∀ gs r, P2 gs r) ∧
    P3 ∧
    (∀ cap page l p, 0 < cap → P4fail true cap page l p) ∧
    (∀ cap page cals, P4merge cap page cals) ∧
    (∀ toUtc ds de m busy start stop days,
      P5 toUtc ds de m busy start stop days (suggestFix toUtc ds de m busy start stop days)) ∧
    (∀ e : Places, e.norm "" = "" → ∀ ev, P6 e ev) :=
  ⟨p1_run, fun e a v w => ⟨fun h => ((p1_switch e a v w).1 h).1, fun h => ((p1_switch e a v w).2 h).1⟩,
   p2, p3, fun cap page l p h => p4_fail_fixed cap page h l p, p4_merge, p5_fixed, p6⟩

/-- The fixed code answers each counterexample right. -/
theorem fixed_replays :
    listEvents true 20 50 .netErr (some ([⟨1, 900⟩], false)) = some ⟨[⟨primaryId, ⟨1, 900⟩⟩], false⟩ ∧
    suggestFix laZone 9 18 60 [] 420 3360 [0, 1] = [(960, 1500), (2460, 3000)] ∧
    suggestFix (fun w => w + 420) 9 18 60 [] 0 1440 [0] = [(960, 1440)] := by
  refine ⟨rfl, by decide +kernel, by decide +kernel⟩

end Eve.CalendarEdits
