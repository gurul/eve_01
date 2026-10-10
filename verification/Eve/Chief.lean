/-
  The chief of staff's card machine (bridge/src/eve_bridge/chief.py, chief_card.py, chief_ledger.py,
  chief_receipt.py, chief_desk.py, chief_reflect.py), written 2026-09-29 before the Python, from the chief
  design (sections 3.3, 4.1, 4.3 and 7) and its build addendum (owner decisions, Reflexion). This is new code
  meant to be right from the start, so the honest shape is `code_invariant`: the properties proved of the
  machine as specified, over every trace. There is no earlier buggy version to put a `current_violates` on.
  `naive_violates` is the positive control: the same machine with an oracle that trusts the executor's
  sentence closes a card as done with no evidence.

  The properties (design 9 P8, addendum "Reflexion"), conjoined in `Spec`:

    DoneNeedsEvidence       a card is done only on the owner's word, or when every done check is confirmed
                            and every confirmation rests on evidence the oracle saw outside the executor's
                            own sentence (design 2.4; plan_executor.py:20-25, "unverifiable is never success");
    OneYesOneAct            every one-way dispatch spends a distinct yes, given for that phase at the card's
                            revision at the time of the dispatch; a no, a hold word or a timeout is never a yes
                            (design 7.3; consent.py:50 `decision`);
    BudgetBeforeDispatch    what the card spent plus the ceiling held by its running phase never exceeds the
                            cap, up to what executors ran past their own ceilings (`over`, 0 when every
                            ceiling holds) (design 7.6);
    PushBudgetQuiet         an interrupting message (an unprompted push, or a reminder the owner asked for)
                            goes out now only outside quiet hours and at a breakpoint (a reminder also once it
                            has waited 45 minutes for one: owner, 2026-10-06), an unprompted one only
                            with pushes on, at most 2 a day, and the day's count survives a restart; no Go is
                            asked and no act or one-way phase starts in quiet hours (design 4.3, 7.7; addendum 1);
    NoActReplayAfterRestart a one-way act in flight at a restart is never dispatched again without a yes given
                            after the restart (design 4.3 "Any restart", 7.13);
    OneWayNeverAutoRetried  a one-way phase's attempt count rises only with a fresh yes, and it is never
                            retried automatically; a two-way phase retries itself at most MAX_RETRIES = 2
                            times (3 trials) (addendum, Reflexion "Trials");
    LessonsBounded          at most OMEGA = 3 lessons per (executor, phase kind) key (addendum, Reflexion
                            "Episodic memory"; Shinn et al., NeurIPS 2023, arXiv:2303.11366, Omega = 1-3).

  Abstraction. One card, the desk it shares with every other card, and the lesson store. Cards share nothing
  else: every card event names its card id, and the budget, revision, approvals and checks are per card, so
  what is proved of one card holds of each card of many. The phase list and the number of done checks are
  fixed when take_on makes the card (`Plan`); a revision bumps `rev` and cancels every approval and every
  pending Go (design 7.3), and does not re-plan the phases here. A card runs one phase at a time (the
  research -> assess -> act chain of design 4.1; each phase reads the previous one's result), so
  `running` is at most one index. Budgets are two Nat counters per dimension (`usd`, `secs`), read the same way
  by the code; the replay test uses integer micro-dollars. Ceilings are a parameter (`ceil`), per phase kind
  (design 7.6: web read 90 s, Mac 10 min, the astra call); every theorem holds for every choice. The door
  floor (a regex over the goal; current act executors can ask at runtime) and the executor choice are outside: `Plan` carries each phase's door after the
  floor, and a door only ever rises (`raise`, add-only, only before the phase first ran). Which Mac floor runs
  an act (Codex forced) and the one Mac slot are unit-tested, not modelled. Time is the minute of the local day
  (`clock`, 0-1439) and the change of date (`newDay`); `start` is at minute 0, inside quiet hours, so a replay
  sends `clock` first. Quiet hours bar every one-way phase as well as every act (design 4.3 names acts only). A Reflexion call that errors writes no lesson and
  retries nothing: it is no event at all.

  Python names the replay test (bridge/tests/test_chief_lean.py) maps to. The design fixes the ledger's event
  kinds and the card and phase fields; where it names nothing (`attempts`, `retries`, the "I may have done"
  state) the name below is the one the replay should find, or bind to whatever P3 ships under it.

    Event (here)              Python
    ask n                     the Go for phase n goes out (`_ask_user(..., choices=YES_NO)`); ledger `asked`
                              {card, n, rev}; a Go the Desk holds back in quiet hours is no event here
    answer n .yes/.no/        the answer to that Go: a Yes tap / a No tap / 180 s of silence / a hold word
      .timeout/.hold          (ledger `answered` with answer "yes"|"no"|"timeout"|"hold"; `approved` on yes)
    goId n                    the code word "go c<N>" (ledger `approved`, via "id")
    revise                    `Ledger.revise(id, patch, why)` / Change (ledger `revised`, rev + 1)
    raise n                   a door raised to one_way before the phase ever ran (ledger `revised`)
    dispatch n                the chief starts phase n (ledger `dispatched`, written before the executor runs)
    result n good cost        `Chief.on_phase_result(card, n, result)`; good = result.status == "done";
                              cost = (result.usd, result.secs)
    reflect n key retry       chief_reflect: a lesson appended to lessons.jsonl under key (executor, phase kind)
                              (ledger `reflected`), and the phase re-queued (`retried`) as Trials allows
    requeue n                 Reopen on one finished phase
    restart cost              the daemon restarts: `Ledger(dir)` reloads, `Chief.run()` resumes; cost = the
                              spend rows tagged with the card for the run the restart killed
    happened n / didnt n      the "It happened" / "It didn't" buttons on "I may have done: ..."
    oracle i ev               `chief_receipt.check(done_checks[i], results)` (ledger `checked`): ev is what it
                              read, verdict "confirmed"|"unverifiable"|"failed"
    close                     `chief_receipt.close(card)` after the last phase
    ownerDone                 "done c<N>", or "Yes, done" on an unverified receipt, or Done on a commitment
    reopen                    Reopen on the receipt (rev + 1, checks back to pending)
    drop                      "drop c<N>" / Drop
    raiseBudget k             the "Raise to $X?" tap
    clock m / newDay          the `now` the Desk is asked with: minutes since local midnight / a new local date
    breakpoint / busy         `Desk.breakpoint(kind)` / the owner acting (`Desk.acted(card_id)`, his message)
    offer k                   `Desk.offer(item)`; k = unprompted (nudge without a cue, evening close, weekly
                              sheet) | reminder (a cue he asked for) | reply | receipt

    State (here)              Python
    card.rev, card.status     card.rev; card.status ("active" and "waiting" are `active`; "proposed" cards
                              are not modelled: they run nothing until kept)
    card.checks[i].1          the latest `checked` verdict of card.done_checks[i] since the last reopen
    card.byOwner              the card was closed by the owner's word (the receipt's "your call")
    card.running              the index of the phase with status "running" in this process, or None
    card.held/cap/spent       the running phase's ceiling / card.budget / card.spent, each {usd, wall seconds}
    phase.kind, phase.door    phase.do, phase.door
    phase.st                  phase.status: queued|running|ok|failed; `asked` = "queued" with an unanswered
                              `asked` event at the current rev (card.waiting_for == "go"); `doubt` = a phase the
                              reload found "running" with no `result` after its last `dispatched`
    phase.token               phase.approval["rev"], or None when phase.approval is None (single use)
    phase.askRev              the rev of the pending `asked` event
    phase.attempts, .retries  the count of `dispatched` / `retried` events for the phase
    desk.mode                 EVE_CHIEF_PUSH (off|shadow|on)
    desk.pushes               `Ledger.pushes_today(now)` (from attention.jsonl: it survives a restart)
    desk.route                `Route.route` of the last `Desk.offer`: now|batch|silent|drop
    lessons                   the keys of lessons.jsonl's lines, oldest first, each key a Nat here

  Ghost fields (`yeses`, `owActs`, `lastYes`, `stale`, `doubt`, `replay`, `over`, and S's `quietAct`,
  `badPush`, `sentToday`) have no Python field; the replay computes them from the ledger's events.
-/
namespace Eve.Chief

/-! ## Constants (addendum 1 and Reflexion) -/

def QUIET_FROM : Nat := 22 * 60 + 30   -- 22:30
def QUIET_TO : Nat := 8 * 60           -- 08:00
def PUSH_BUDGET : Nat := 2             -- unprompted pushes a day
def MAX_RETRIES : Nat := 2             -- automatic retries of a two-way phase (3 trials)
def OMEGA : Nat := 3                   -- lessons kept per (executor, phase kind)

def quiet (m : Nat) : Bool := decide (QUIET_FROM ≤ m) || decide (m < QUIET_TO)

/-! ## Closed sets -/

inductive Door where
  | twoWay
  | oneWay
  deriving DecidableEq, Repr

/-- The live phase kinds tonight (addendum "Scope"). -/
inductive Kind where
  | research
  | assess
  | act
  | watch
  deriving DecidableEq, Repr

inductive PSt where
  | queued
  | asked      -- a Go for it is out, unanswered
  | running
  | ok
  | failed
  | doubt      -- a one-way act was in flight at a restart: "I may have done"
  deriving DecidableEq, Repr

inductive CSt where
  | active
  | unverified
  | done
  | dropped
  deriving DecidableEq, Repr

inductive Verdict where
  | pending
  | confirmed
  | unverifiable
  | failed
  deriving DecidableEq, Repr

/-- What an oracle read. `executorText` is the executor's own sentence: data, never evidence (design 2.4). -/
inductive Evidence where
  | observed       -- links read, UI text seen, the watch row, the file: outside the executor's sentence
  | contradicted   -- the evidence says it did not happen
  | executorText
  | missing
  deriving DecidableEq, Repr

inductive Answer where
  | yes
  | no
  | timeout
  | hold
  deriving DecidableEq, Repr

inductive Item where
  | unprompted
  | reminder
  | reply
  | receipt
  deriving DecidableEq, Repr

inductive Mode where
  | off
  | shadow
  | on
  deriving DecidableEq, Repr

inductive Route where
  | now
  | batch
  | silent
  | drop
  deriving DecidableEq, Repr

structure Cost where
  usd : Nat
  secs : Nat
  deriving DecidableEq, Repr

inductive Event where
  | ask (n : Nat)
  | answer (n : Nat) (a : Answer)
  | goId (n : Nat)
  | revise
  | raise (n : Nat)
  | dispatch (n : Nat)
  | result (n : Nat) (good : Bool) (cost : Cost)
  | reflect (n : Nat) (key : Nat) (retry : Bool)
  | requeue (n : Nat)
  | restart (cost : Cost)
  | happened (n : Nat)
  | didnt (n : Nat)
  | oracle (i : Nat) (ev : Evidence)
  | close
  | ownerDone
  | reopen
  | drop
  | raiseBudget (k : Cost)
  | clock (m : Nat)
  | newDay
  | breakpoint
  | waited                -- an asked reminder's 45 minutes ran out with no breakpoint (chief_desk REMINDER_WAIT_SECS)
  | busy
  | offer (k : Item)
  deriving DecidableEq, Repr

/-! ## State -/

structure Phase where
  kind : Kind
  door : Door
  st : PSt := .queued
  token : Option Nat := none     -- approval: the rev it was given at; single use
  askRev : Option Nat := none    -- the rev of the Go that is out
  attempts : Nat := 0
  retries : Nat := 0
  yeses : Nat := 0               -- ghost: yeses given for this phase
  owActs : Nat := 0              -- ghost: dispatches while one_way
  lastYes : Option Nat := none   -- ghost: the rev of the latest yes (a revision does not clear it)
  stale : Bool := false          -- ghost: a one-way dispatch whose latest yes was for another revision
  doubt : Bool := false          -- ghost: in flight at a restart, no yes since
  replay : Bool := false         -- ghost: dispatched while `doubt`
  deriving DecidableEq, Repr

structure Card where
  rev : Nat := 1
  status : CSt := .active
  phases : List Phase := []
  checks : List (Verdict × Bool) := []   -- the stored verdict; ghost: the oracle saw `observed`
  byOwner : Bool := false
  running : Option Nat := none
  held : Cost := ⟨0, 0⟩
  cap : Cost
  spent : Cost := ⟨0, 0⟩
  over : Cost := ⟨0, 0⟩                  -- ghost: what executors ran past their ceilings
  deriving DecidableEq, Repr

structure Desk where
  mode : Mode := .shadow
  clock : Nat := 0
  atBreak : Bool := false
  waited : Bool := false
  pushes : Nat := 0
  route : Route := .batch
  deriving DecidableEq, Repr

structure S where
  card : Card
  desk : Desk := {}
  lessons : List Nat := []
  quietAct : Bool := false   -- ghost: a Go asked, or an act / one-way phase started, in quiet hours
  badPush : Bool := false    -- ghost: an interrupting message went out now against its rule
  sentToday : Nat := 0       -- ghost: unprompted messages that went out now since the date changed
  deriving DecidableEq, Repr

/-- What take_on made: the phases (after the door floor), the number of done checks, the budget. -/
structure Plan where
  phases : List (Kind × Door)
  checks : Nat
  cap : Cost
  mode : Mode
  deriving DecidableEq, Repr

def start (p : Plan) : S :=
  { card := { phases := p.phases.map (fun kd => { kind := kd.1, door := kd.2 }),
              checks := List.replicate p.checks (.pending, false), cap := p.cap },
    desk := { mode := p.mode } }

/-- Apply `f` to the element at `n`, if there is one. -/
def upd {α : Type} (f : α → α) : Nat → List α → List α
  | _, [] => []
  | 0, a :: l => f a :: l
  | n + 1, a :: l => a :: upd f n l

/-! ## The code: phases (chief.py `handle`, `on_phase_result`, resume; chief_reflect.py) -/

/-- A yes for this phase at revision `r`: single-use approval bound to (card, n, r) (design 7.3). -/
def approve (r : Nat) (p : Phase) : Phase :=
  { p with st := .queued, askRev := none, token := some r, yeses := p.yeses + 1, lastYes := some r,
           doubt := false }

def fAsk (r : Nat) (p : Phase) : Phase :=
  if p.door = .oneWay ∧ p.st = .queued ∧ p.token = none then { p with st := .asked, askRev := some r } else p

/-- Only a Yes to the Go that is out, at the card's revision, approves; a no, a hold word and a timeout all
put the phase back to waiting for a Go with nothing approved (consent.py:50; design 4.1 step 7). -/
def fAnswer (r : Nat) (a : Answer) (p : Phase) : Phase :=
  if p.st = .asked then
    (if a = .yes ∧ p.askRev = some r ∧ p.door = .oneWay then approve r p
     else { p with st := .queued, askRev := none })
  else p

/-- "go c<N>": code matches the id and approves that card's waiting act at its current revision. -/
def fGo (r : Nat) (p : Phase) : Phase :=
  if p.door = .oneWay ∧ (p.st = .queued ∨ p.st = .asked) then approve r p else p

/-- A revision, a reopen or a drop cancels every approval and every pending Go. -/
def fClear (p : Phase) : Phase :=
  { p with token := none, askRev := none, st := if p.st = .asked then .queued else p.st }

/-- The door is add-only, and set before the phase first runs. -/
def fRaise (p : Phase) : Phase :=
  if p.st = .queued ∧ p.attempts = 0 then { p with door := .oneWay } else p

def canGo (r : Nat) (p : Phase) : Bool := p.st == .queued && (p.door == .twoWay || p.token == some r)

/-- An act or a one-way phase never starts in quiet hours (design 4.3). -/
def quietBars (p : Phase) : Bool := p.door == .oneWay || p.kind == .act

/-- The dispatch spends the approval (it is written to the ledger before the act). -/
def fDispatch (r : Nat) (p : Phase) : Phase :=
  if canGo r p then
    { p with st := .running, token := none, attempts := p.attempts + 1,
             owActs := if p.door = .oneWay then p.owActs + 1 else p.owActs,
             stale := p.stale || (p.door == .oneWay && p.lastYes != some r),
             replay := p.replay || (p.door == .oneWay && p.doubt) }
  else p

def fResult (good : Bool) (p : Phase) : Phase :=
  if p.st = .running then { p with st := if good then .ok else .failed } else p

/-- Reflexion's trials (addendum): a failed one-way act goes back to waiting for a new Go (the Go shows the
lesson); a failed two-way phase is re-queued with its lessons while it has retries left. -/
def fReflect (retry : Bool) (p : Phase) : Phase :=
  if p.st = .failed then
    (if p.door = .oneWay then { p with st := .queued }
     else if retry = true ∧ p.retries < MAX_RETRIES then { p with st := .queued, retries := p.retries + 1 }
     else p)
  else p

def fRequeue (p : Phase) : Phase := if p.st = .ok ∨ p.st = .failed then { p with st := .queued } else p

/-- Resume (design 4.3 "Any restart"): a read-only phase runs again; an act in flight waits for the owner
("I may have done"); a Go that was out died with the process. An approval not yet spent stays in the ledger. -/
def fRestart (p : Phase) : Phase :=
  if p.st = .running then (if p.door = .oneWay then { p with st := .doubt, doubt := true } else { p with st := .queued })
  else if p.st = .asked then { p with st := .queued, askRev := none }
  else p

def fHappened (p : Phase) : Phase := if p.st = .doubt then { p with st := .ok } else p
def fDidnt (p : Phase) : Phase := if p.st = .doubt then { p with st := .queued } else p

/-! ## The code: the card (chief.py, chief_receipt.py) -/

/-- chief_receipt.check: the verdict from what the oracle read. -/
def verdictOf : Evidence → Verdict
  | .observed => .confirmed
  | .contradicted => .failed
  | .executorText => .unverifiable
  | .missing => .unverifiable

def fits (c : Card) (k : Cost) : Bool :=
  decide (c.spent.usd + c.held.usd + k.usd ≤ c.cap.usd) && decide (c.spent.secs + c.held.secs + k.secs ≤ c.cap.secs)

/-- The running phase ends (a result, or the restart that killed it): its spend is added, its ceiling freed. -/
def charge (c : Card) (k : Cost) : Card :=
  { c with running := none, held := ⟨0, 0⟩,
           spent := ⟨c.spent.usd + k.usd, c.spent.secs + k.secs⟩,
           over := ⟨c.over.usd + (k.usd - c.held.usd), c.over.secs + (k.secs - c.held.secs)⟩ }

def isActive (c : Card) : Bool := c.status == .active

def onPhase (c : Card) (n : Nat) (f : Phase → Phase) : Card := { c with phases := upd f n c.phases }

def allConfirmed (l : List (Verdict × Bool)) : Bool := !l.isEmpty && l.all (fun x => x.1 == .confirmed)

/-- The card's step. `vf` is the oracle's verdict function (`verdictOf` in the code); `quiet` is the Desk's
reading of the clock. -/
def cardStep (vf : Evidence → Verdict) (ceil : Kind → Cost) (q : Bool) (c : Card) : Event → Card
  | .ask n => if isActive c && !q then onPhase c n (fAsk c.rev) else c
  | .answer n a => if isActive c then onPhase c n (fAnswer c.rev a) else c
  | .goId n => if isActive c then onPhase c n (fGo c.rev) else c
  | .revise => if isActive c then { c with rev := c.rev + 1, phases := c.phases.map fClear } else c
  | .raise n => if isActive c then onPhase c n fRaise else c
  | .dispatch n => match c.phases[n]? with
      | some p =>
          if isActive c && c.running == none && canGo c.rev p && fits c (ceil p.kind) && !(q && quietBars p) then
            { c with phases := upd (fDispatch c.rev) n c.phases, running := some n, held := ceil p.kind }
          else c
      | none => c
  | .result n good k => if c.running == some n then onPhase (charge c k) n (fResult good) else c
  | .reflect n _ retry => onPhase c n (fReflect retry)
  | .requeue n => if isActive c then onPhase c n fRequeue else c
  | .restart k =>
      let c' := if c.running == none then c else charge c k
      { c' with phases := c'.phases.map fRestart }
  | .happened n => onPhase c n fHappened
  | .didnt n => onPhase c n fDidnt
  | .oracle i ev => if isActive c then { c with checks := upd (fun _ => (vf ev, ev == .observed)) i c.checks } else c
  | .close => if isActive c && c.running == none then
        { c with status := if allConfirmed c.checks then .done else .unverified } else c
  | .ownerDone => if c.status == .active || c.status == .unverified then { c with status := .done, byOwner := true }
      else c
  | .reopen => if c.status == .done || c.status == .unverified then
        { c with status := .active, rev := c.rev + 1, checks := c.checks.map (fun _ => (.pending, false)),
                 byOwner := false, phases := c.phases.map fClear }
      else c
  | .drop => if c.status != .dropped then { c with status := .dropped, phases := c.phases.map fClear } else c
  | .raiseBudget k => { c with cap := ⟨c.cap.usd + k.usd, c.cap.secs + k.secs⟩ }
  | .clock _ => c
  | .newDay => c
  | .breakpoint => c
  | .waited => c
  | .busy => c
  | .offer _ => c

/-! ## The code: the Desk (chief_desk.py `offer`) and the lessons (chief_reflect.py) -/

def interrupting : Item → Bool
  | .unprompted => true
  | .reminder => true
  | _ => false

/-- `Desk.offer`: replies and receipts go now; a reminder the owner asked for waits for a breakpoint outside
quiet hours, or goes once it has waited 45 minutes for one (owner, 2026-10-06), still never in quiet hours; an unprompted message is dropped (off), logged as would_push (shadow), or goes now only
outside quiet hours, at a breakpoint and within the day's budget (on); every other case is batch. -/
def offer (d : Desk) : Item → Desk
  | .reply => { d with route := .now }
  | .receipt => { d with route := .now }
  | .reminder => if !quiet d.clock && (d.atBreak || d.waited) then { d with route := .now }
      else { d with route := .batch }
  | .unprompted => match d.mode with
      | .off => { d with route := .drop }
      | .shadow => { d with route := .silent }
      | .on => if !quiet d.clock && d.atBreak && decide (d.pushes < PUSH_BUDGET) then
                 { d with route := .now, pushes := d.pushes + 1 }
               else { d with route := .batch }

def deskStep (d : Desk) : Event → Desk
  | .clock m => { d with clock := m % 1440 }
  | .newDay => { d with pushes := 0 }
  | .breakpoint => { d with atBreak := true }
  | .waited => { d with waited := true }
  | .busy => { d with atBreak := false }
  | .restart _ => { d with atBreak := false }
  | .offer k => offer d k
  | _ => d

/-- lessons.jsonl: append, then drop the oldest line of that key while it has more than OMEGA. -/
def addLesson (k : Nat) (l : List Nat) : List Nat :=
  if OMEGA < (l ++ [k]).count k then (l ++ [k]).erase k else l ++ [k]

/-- A lesson is written only for a phase that came back failed or unverifiable (addendum). -/
def lessonStep (c : Card) (l : List Nat) : Event → List Nat
  | .reflect n k _ => match c.phases[n]? with
      | some p => if p.st == .failed then addLesson k l else l
      | none => l
  | _ => l

/-! ## Observations (ghosts): what went out, read off the code's output -/

/-- A Go was asked (phase n went to `asked`), or an act or a one-way phase was started. -/
def actStarted (c c' : Card) : Event → Bool
  | .dispatch n => c'.running == some n && c.running == none && ((c.phases[n]?).map quietBars) == some true
  | .ask n => ((c'.phases[n]?).map (·.st)) == some .asked && ((c.phases[n]?).map (·.st)) != some .asked
  | _ => false

/-- An interrupting message went out now against its rule. -/
def pushedAgainstRule (d d' : Desk) : Event → Bool
  | .offer k => d'.route == .now && interrupting k &&
      (quiet d.clock || (!d.atBreak && !(k == .reminder && d.waited)) || (k == .unprompted && d.mode != .on))
  | _ => false

def sentNow (d' : Desk) : Event → Nat
  | .offer .unprompted => if d'.route == .now then 1 else 0
  | _ => 0

def stepWith (vf : Evidence → Verdict) (ceil : Kind → Cost) (s : S) (e : Event) : S :=
  let q := quiet s.desk.clock
  let c' := cardStep vf ceil q s.card e
  let d' := deskStep s.desk e
  { card := c', desk := d', lessons := lessonStep s.card s.lessons e,
    quietAct := s.quietAct || (q && actStarted s.card c' e),
    badPush := s.badPush || pushedAgainstRule s.desk d' e,
    sentToday := if e = .newDay then 0 else s.sentToday + sentNow d' e }

def step (ceil : Kind → Cost) : S → Event → S := stepWith verdictOf ceil

def run (ceil : Kind → Cost) (p : Plan) (evs : List Event) : S := evs.foldl (step ceil) (start p)

/-! ## The properties -/

def DoneNeedsEvidence (s : S) : Bool :=
  s.card.status != .done || s.card.byOwner || (!s.card.checks.isEmpty && s.card.checks.all (·.2))

def OneYesOneAct (s : S) : Bool :=
  s.card.phases.all (fun p => decide (p.owActs ≤ p.yeses) && !p.stale)

def BudgetBeforeDispatch (s : S) : Bool :=
  decide (s.card.spent.usd + s.card.held.usd ≤ s.card.cap.usd + s.card.over.usd) &&
  decide (s.card.spent.secs + s.card.held.secs ≤ s.card.cap.secs + s.card.over.secs)

def PushBudgetQuiet (s : S) : Bool :=
  !s.badPush && !s.quietAct && decide (s.sentToday ≤ PUSH_BUDGET) && s.desk.pushes == s.sentToday

def NoActReplayAfterRestart (s : S) : Bool := s.card.phases.all (fun p => !p.replay)

def OneWayNeverAutoRetried (s : S) : Bool :=
  s.card.phases.all (fun p =>
    (p.door != .oneWay || (decide (p.attempts ≤ p.yeses) && p.retries == 0)) && decide (p.retries ≤ MAX_RETRIES))

def LessonsBounded (s : S) : Bool := s.lessons.all (fun k => decide (s.lessons.count k ≤ OMEGA))

def Spec (s : S) : Bool :=
  DoneNeedsEvidence s && OneYesOneAct s && BudgetBeforeDispatch s && PushBudgetQuiet s &&
  NoActReplayAfterRestart s && OneWayNeverAutoRetried s && LessonsBounded s

/-! ## The inductive invariant -/

structure PInv (r : Nat) (p : Phase) : Prop where
  bind : ∀ t, p.token = some t → t = r ∧ p.lastYes = some t
  cnt : p.owActs ≤ p.yeses
  cntTok : p.token ≠ none → p.owActs < p.yeses
  ow : p.door = .oneWay → p.attempts = p.owActs ∧ p.retries = 0
  tw : p.door = .twoWay → p.owActs = 0
  retr : p.retries ≤ p.attempts
  retrMax : p.retries ≤ MAX_RETRIES
  fail : p.st = .failed → p.retries < p.attempts
  run : p.st = .running → p.token = none ∧ p.retries < p.attempts
  dbt : p.doubt = true → p.token = none
  stale : p.stale = false
  replay : p.replay = false

structure CInv (c : Card) : Prop where
  phases : ∀ p ∈ c.phases, PInv c.rev p
  checks : ∀ x ∈ c.checks, x.1 = .confirmed → x.2 = true
  done : c.status = .done → c.byOwner = true ∨ (c.checks ≠ [] ∧ ∀ x ∈ c.checks, x.2 = true)
  idle : c.running = none → c.held.usd = 0 ∧ c.held.secs = 0
  usd : c.spent.usd + c.held.usd ≤ c.cap.usd + c.over.usd
  secs : c.spent.secs + c.held.secs ≤ c.cap.secs + c.over.secs

structure Inv (s : S) : Prop where
  card : CInv s.card
  lessons : ∀ k, s.lessons.count k ≤ OMEGA
  pushes : s.desk.pushes = s.sentToday
  budget : s.sentToday ≤ PUSH_BUDGET
  badPush : s.badPush = false
  quietAct : s.quietAct = false

/-! ### Lists -/

theorem mem_upd {α : Type} (f : α → α) :
    ∀ (n : Nat) (l : List α) (q : α), q ∈ upd f n l → q ∈ l ∨ ∃ a ∈ l, q = f a
  | _, [], _, h => by simp [upd] at h
  | 0, a :: l, q, h => by
      simp only [upd, List.mem_cons] at h
      rcases h with h | h
      · exact Or.inr ⟨a, List.mem_cons_self .., h⟩
      · exact Or.inl (List.mem_cons_of_mem _ h)
  | n + 1, a :: l, q, h => by
      simp only [upd, List.mem_cons] at h
      rcases h with h | h
      · exact Or.inl (h ▸ List.mem_cons_self ..)
      · rcases mem_upd f n l q h with h | ⟨b, hb, rfl⟩
        · exact Or.inl (List.mem_cons_of_mem _ h)
        · exact Or.inr ⟨b, List.mem_cons_of_mem _ hb, rfl⟩

theorem all_upd {α : Type} (P : α → Prop) (f : α → α) (n : Nat) (l : List α)
    (hf : ∀ a, P a → P (f a)) (h : ∀ a ∈ l, P a) : ∀ q ∈ upd f n l, P q := by
  intro q hq
  rcases mem_upd f n l q hq with hq | ⟨a, ha, rfl⟩
  · exact h q hq
  · exact hf a (h a ha)

theorem all_map' {α : Type} (P Q : α → Prop) (f : α → α) (l : List α)
    (hf : ∀ a, P a → Q (f a)) (h : ∀ a ∈ l, P a) : ∀ q ∈ l.map f, Q q := by
  intro q hq
  obtain ⟨a, ha, rfl⟩ := List.mem_map.mp hq
  exact hf a (h a ha)

theorem get_mem {α : Type} (l : List α) (n : Nat) (a : α) (h : l[n]? = some a) : a ∈ l :=
  List.mem_of_getElem? h

/-! ### Each phase transformer keeps `PInv` -/

theorem pinv_approve (r : Nat) (p : Phase) (h : PInv r p) : PInv r (approve r p) := by
  have := h.cnt; have := h.cntTok
  refine ⟨?_, ?_, ?_, h.ow, h.tw, h.retr, h.retrMax, ?_, ?_, ?_, h.stale, h.replay⟩ <;> simp [approve]
  · omega
  · cases hp : p.token with
    | none => omega
    | some t => have := h.cntTok (by simp [hp]); omega

theorem pinv_fAsk (r : Nat) (p : Phase) (h : PInv r p) : PInv r (fAsk r p) := by
  unfold fAsk; split
  · rename_i hc
    exact ⟨h.bind, h.cnt, h.cntTok, h.ow, h.tw, h.retr, h.retrMax, by simp, by simp, h.dbt, h.stale, h.replay⟩
  · exact h

theorem pinv_fAnswer (r : Nat) (a : Answer) (p : Phase) (h : PInv r p) : PInv r (fAnswer r a p) := by
  unfold fAnswer; split
  · split
    · exact pinv_approve r p h
    · exact ⟨h.bind, h.cnt, h.cntTok, h.ow, h.tw, h.retr, h.retrMax, by simp, by simp, h.dbt, h.stale, h.replay⟩
  · exact h

theorem pinv_fGo (r : Nat) (p : Phase) (h : PInv r p) : PInv r (fGo r p) := by
  unfold fGo; split
  · exact pinv_approve r p h
  · exact h

theorem pinv_fClear (r r' : Nat) (p : Phase) (h : PInv r p) : PInv r' (fClear p) := by
  refine ⟨by simp [fClear], h.cnt, by simp [fClear], h.ow, h.tw, h.retr, h.retrMax, ?_, ?_, by simp [fClear],
    h.stale, h.replay⟩
  · have hf := h.fail
    intro h'; simp only [fClear] at h' ⊢; cases hs : p.st <;> simp_all
  · have hr := h.run
    intro h'; simp only [fClear] at h' ⊢; cases hs : p.st <;> simp_all

theorem pinv_fRaise (r : Nat) (p : Phase) (h : PInv r p) : PInv r (fRaise p) := by
  unfold fRaise; split
  · rename_i hc
    obtain ⟨_, ha⟩ := hc
    have hr := h.retr
    have ho : p.owActs = 0 := by
      cases hd : p.door with
      | twoWay => exact h.tw hd
      | oneWay => have := (h.ow hd).1; omega
    exact ⟨h.bind, h.cnt, h.cntTok, fun _ => ⟨by simp [ha, ho], by simp; omega⟩, by simp, h.retr, h.retrMax,
      h.fail, h.run, h.dbt, h.stale, h.replay⟩
  · exact h

theorem pinv_fDispatch (r : Nat) (p : Phase) (h : PInv r p) : PInv r (fDispatch r p) := by
  unfold fDispatch; split
  · rename_i hc
    simp only [canGo, Bool.and_eq_true, beq_iff_eq, Bool.or_eq_true] at hc
    obtain ⟨_, hd⟩ := hc
    have hr := h.retr
    cases hdoor : p.door with
    | twoWay =>
        have ho := h.tw hdoor
        have := h.cnt
        refine ⟨by simp, by simp; omega, by simp, by simp, fun _ => by simp [ho], by simp; omega,
          h.retrMax, by simp, fun _ => ⟨rfl, by simp; omega⟩, by simp, ?_, ?_⟩
        · simp [h.stale]
        · simp [h.replay]
    | oneWay =>
        have ht : p.token = some r := by
          rcases hd with hd | hd
          · rw [hdoor] at hd; cases hd
          · exact hd
        have hb := (h.bind r ht).2
        have hlt := h.cntTok (by simp [ht])
        have hdb : p.doubt = false := by
          cases hx : p.doubt with
          | false => rfl
          | true => have := h.dbt hx; rw [ht] at this; cases this
        obtain ⟨ha, hre⟩ := h.ow hdoor
        refine ⟨by simp, by simp; omega, by simp, fun _ => ⟨by simp; omega, by simp [hre]⟩,
          fun h' => by simp at h', by simp; omega, h.retrMax, by simp, fun _ => ⟨rfl, by simp; omega⟩, by simp,
          ?_, ?_⟩
        · simp [hb, h.stale]
        · simp [hdb, h.replay]
  · exact h

theorem pinv_fResult (r : Nat) (good : Bool) (p : Phase) (h : PInv r p) : PInv r (fResult good p) := by
  unfold fResult; split
  · rename_i hs
    have hrun := h.run hs
    refine ⟨h.bind, h.cnt, h.cntTok, h.ow, h.tw, h.retr, h.retrMax, fun _ => hrun.2, ?_, h.dbt, h.stale, h.replay⟩
    intro h'; cases good <;> simp at h'
  · exact h

theorem pinv_fReflect (r : Nat) (retry : Bool) (p : Phase) (h : PInv r p) : PInv r (fReflect retry p) := by
  unfold fReflect; split
  · rename_i hs
    split
    · exact ⟨h.bind, h.cnt, h.cntTok, h.ow, h.tw, h.retr, h.retrMax, by simp, by simp, h.dbt, h.stale, h.replay⟩
    · rename_i hd
      split
      · rename_i hc
        have hf := h.fail hs
        refine ⟨h.bind, h.cnt, h.cntTok, fun h' => absurd h' hd, h.tw, by simp; omega, by simp; omega, by simp,
          by simp, h.dbt, h.stale, h.replay⟩
      · exact h
  · exact h

theorem pinv_fRequeue (r : Nat) (p : Phase) (h : PInv r p) : PInv r (fRequeue p) := by
  unfold fRequeue; split
  · exact ⟨h.bind, h.cnt, h.cntTok, h.ow, h.tw, h.retr, h.retrMax, by simp, by simp, h.dbt, h.stale, h.replay⟩
  · exact h

theorem pinv_fRestart (r : Nat) (p : Phase) (h : PInv r p) : PInv r (fRestart p) := by
  unfold fRestart; split
  · rename_i hs
    have ht := (h.run hs).1
    split
    · exact ⟨h.bind, h.cnt, h.cntTok, h.ow, h.tw, h.retr, h.retrMax, by simp, by simp, fun _ => ht, h.stale,
        h.replay⟩
    · exact ⟨h.bind, h.cnt, h.cntTok, h.ow, h.tw, h.retr, h.retrMax, by simp, by simp, h.dbt, h.stale, h.replay⟩
  · split
    · exact ⟨h.bind, h.cnt, h.cntTok, h.ow, h.tw, h.retr, h.retrMax, by simp, by simp, h.dbt, h.stale, h.replay⟩
    · exact h

theorem pinv_fHappened (r : Nat) (p : Phase) (h : PInv r p) : PInv r (fHappened p) := by
  unfold fHappened; split
  · exact ⟨h.bind, h.cnt, h.cntTok, h.ow, h.tw, h.retr, h.retrMax, by simp, by simp, h.dbt, h.stale, h.replay⟩
  · exact h

theorem pinv_fDidnt (r : Nat) (p : Phase) (h : PInv r p) : PInv r (fDidnt p) := by
  unfold fDidnt; split
  · exact ⟨h.bind, h.cnt, h.cntTok, h.ow, h.tw, h.retr, h.retrMax, by simp, by simp, h.dbt, h.stale, h.replay⟩
  · exact h

/-! ### The card -/

/-- A card whose phases went through `f` and nothing the invariant reads besides changed. -/
theorem cinv_onPhase (c : Card) (n : Nat) (f : Phase → Phase) (h : CInv c)
    (hf : ∀ p, PInv c.rev p → PInv c.rev (f p)) : CInv (onPhase c n f) :=
  ⟨all_upd _ f n c.phases hf h.phases, h.checks, h.done, h.idle, h.usd, h.secs⟩

theorem cinv_charge (c : Card) (k : Cost) (h : CInv c) : CInv (charge c k) := by
  have := h.usd; have := h.secs
  exact ⟨h.phases, h.checks, h.done, fun _ => ⟨rfl, rfl⟩, by simp [charge]; omega, by simp [charge]; omega⟩

theorem cinv_step (vf : Evidence → Verdict) (hvf : ∀ ev, vf ev = .confirmed → ev = .observed)
    (ceil : Kind → Cost) (q : Bool) (c : Card) (e : Event) (h : CInv c) : CInv (cardStep vf ceil q c e) := by
  cases e with
  | ask n => simp only [cardStep]; split
             · exact cinv_onPhase _ _ _ h (pinv_fAsk c.rev)
             · exact h
  | answer n a => simp only [cardStep]; split
                  · exact cinv_onPhase _ _ _ h (pinv_fAnswer c.rev a)
                  · exact h
  | goId n => simp only [cardStep]; split
              · exact cinv_onPhase _ _ _ h (pinv_fGo c.rev)
              · exact h
  | revise =>
      simp only [cardStep]; split
      · exact ⟨all_map' _ _ fClear _ (pinv_fClear c.rev (c.rev + 1)) h.phases, h.checks, h.done, h.idle,
          h.usd, h.secs⟩
      · exact h
  | raise n => simp only [cardStep]; split
               · exact cinv_onPhase _ _ _ h (pinv_fRaise c.rev)
               · exact h
  | dispatch n =>
      simp only [cardStep]
      split
      · rename_i p hp
        split
        · rename_i hc
          simp only [isActive, fits, Bool.and_eq_true, beq_iff_eq, decide_eq_true_eq] at hc
          obtain ⟨⟨⟨⟨_, hrun⟩, _⟩, hu, hs⟩, _⟩ := hc
          have hidle := h.idle hrun
          exact ⟨all_upd _ _ n c.phases (pinv_fDispatch c.rev) h.phases, h.checks, h.done,
            fun h' => by simp at h', by simp; omega, by simp; omega⟩
        · exact h
      · exact h
  | result n good k =>
      simp only [cardStep]; split
      · exact cinv_onPhase _ _ _ (cinv_charge c k h) (pinv_fResult c.rev good)
      · exact h
  | reflect n key retry => exact cinv_onPhase _ _ _ h (pinv_fReflect c.rev retry)
  | requeue n => simp only [cardStep]; split
                 · exact cinv_onPhase _ _ _ h (pinv_fRequeue c.rev)
                 · exact h
  | restart k =>
      simp only [cardStep]
      have h' : CInv (if (c.running == none) = true then c else charge c k) := by
        split
        · exact h
        · exact cinv_charge c k h
      exact ⟨all_map' _ _ fRestart _ (pinv_fRestart _) h'.phases, h'.checks, h'.done, h'.idle, h'.usd, h'.secs⟩
  | happened n => exact cinv_onPhase _ _ _ h (pinv_fHappened c.rev)
  | didnt n => exact cinv_onPhase _ _ _ h (pinv_fDidnt c.rev)
  | oracle i ev =>
      simp only [cardStep]; split
      · rename_i ha
        have hnd : c.status ≠ .done := by
          intro hd; simp [isActive, hd] at ha
        refine ⟨h.phases, ?_, fun hd => absurd hd hnd, h.idle, h.usd, h.secs⟩
        exact all_upd (fun x => x.1 = .confirmed → x.2 = true) (fun _ => (vf ev, ev == .observed)) i c.checks
          (fun _ _ hc => by simp only at hc ⊢; have := hvf ev hc; simp [this]) h.checks
      · exact h
  | close =>
      simp only [cardStep]; split
      · refine ⟨h.phases, h.checks, ?_, h.idle, h.usd, h.secs⟩
        intro hd
        simp only at hd
        split at hd
        · rename_i hall
          simp only [allConfirmed, Bool.and_eq_true, Bool.not_eq_true', List.isEmpty_eq_false_iff,
            List.all_eq_true, beq_iff_eq] at hall
          exact Or.inr ⟨hall.1, fun x hx => h.checks x hx (hall.2 x hx)⟩
        · cases hd
      · exact h
  | ownerDone =>
      simp only [cardStep]; split
      · exact ⟨h.phases, h.checks, fun _ => Or.inl rfl, h.idle, h.usd, h.secs⟩
      · exact h
  | reopen =>
      simp only [cardStep]; split
      · refine ⟨all_map' _ _ fClear _ (pinv_fClear c.rev (c.rev + 1)) h.phases, ?_, by simp, h.idle, h.usd, h.secs⟩
        intro x hx; obtain ⟨_, _, rfl⟩ := List.mem_map.mp hx; simp
      · exact h
  | drop =>
      simp only [cardStep]; split
      · exact ⟨all_map' _ _ fClear _ (pinv_fClear c.rev c.rev) h.phases, h.checks, by simp, h.idle, h.usd, h.secs⟩
      · exact h
  | raiseBudget k =>
      have := h.usd; have := h.secs
      exact ⟨h.phases, h.checks, h.done, h.idle, by simp [cardStep]; omega, by simp [cardStep]; omega⟩
  | clock m => exact h
  | newDay => exact h
  | breakpoint => exact h
  | waited => exact h
  | busy => exact h
  | offer k => exact h

/-! ### Lessons, the Desk, and the whole state -/

theorem lessons_add (k : Nat) (l : List Nat) (h : ∀ j, l.count j ≤ OMEGA) : ∀ j, (addLesson k l).count j ≤ OMEGA := by
  intro j
  by_cases hkj : k = j
  · subst hkj
    have h1 : (l ++ [k]).count k = l.count k + 1 := by simp [List.count_append]
    have := h k
    unfold addLesson
    split
    · rw [List.count_erase_self, h1]; omega
    · rename_i hc; omega
  · have h1 : (l ++ [k]).count j = l.count j := by simp [List.count_append, hkj]
    have h2 : ((l ++ [k]).erase k).count j = (l ++ [k]).count j := List.count_erase_of_ne (Ne.symm hkj)
    unfold addLesson
    split
    · rw [h2, h1]; exact h j
    · rw [h1]; exact h j

theorem lessons_step (c : Card) (l : List Nat) (e : Event) (h : ∀ j, l.count j ≤ OMEGA) :
    ∀ j, (lessonStep c l e).count j ≤ OMEGA := by
  cases e with
  | reflect n k retry =>
      simp only [lessonStep]
      split
      · split
        · exact lessons_add k l h
        · exact h
      · exact h
  | _ => exact h

theorem offer_ok (d : Desk) (k : Item) (hb : d.pushes ≤ PUSH_BUDGET) :
    (offer d k).pushes = d.pushes + sentNow (offer d k) (.offer k) ∧ (offer d k).pushes ≤ PUSH_BUDGET ∧
    pushedAgainstRule d (offer d k) (.offer k) = false := by
  rcases d with ⟨m, cl, b, w, p, r⟩
  simp only at hb
  cases k with
  | reply => simp [offer, sentNow, pushedAgainstRule, interrupting]; exact hb
  | receipt => simp [offer, sentNow, pushedAgainstRule, interrupting]; exact hb
  | reminder =>
      simp only [offer]
      split
      · rename_i hc
        simp only [Bool.and_eq_true, Bool.not_eq_true', Bool.or_eq_true] at hc
        simp [sentNow, pushedAgainstRule, interrupting, hc.1]
        exact ⟨hb, fun hf => by cases hc.2 with | inl h => simp [hf] at h | inr h => exact h⟩
      · simp [sentNow, pushedAgainstRule]; exact hb
  | unprompted =>
      cases m with
      | off => simp [offer, sentNow, pushedAgainstRule]; exact hb
      | shadow => simp [offer, sentNow, pushedAgainstRule]; exact hb
      | on =>
          simp only [offer]
          split
          · rename_i hc
            simp only [Bool.and_eq_true, Bool.not_eq_true', decide_eq_true_eq] at hc
            simp [sentNow, pushedAgainstRule, interrupting, hc.1.1, hc.1.2]; omega
          · simp [sentNow, pushedAgainstRule]; exact hb

theorem desk_ok (d : Desk) (e : Event) (n : Nat) (hp : d.pushes = n) (hb : n ≤ PUSH_BUDGET) :
    (deskStep d e).pushes = (if e = .newDay then 0 else n + sentNow (deskStep d e) e) ∧
    (if e = .newDay then 0 else n + sentNow (deskStep d e) e) ≤ PUSH_BUDGET ∧
    pushedAgainstRule d (deskStep d e) e = false := by
  cases e with
  | offer k =>
      have := offer_ok d k (hp ▸ hb)
      simp only [deskStep, reduceCtorEq, ↓reduceIte]
      rw [← hp]; exact ⟨this.1, this.1 ▸ this.2.1, this.2.2⟩
  | newDay => simp [deskStep, pushedAgainstRule]
  | _ => simp [deskStep, sentNow, pushedAgainstRule, hp, hb]

theorem inv_step (vf : Evidence → Verdict) (hvf : ∀ ev, vf ev = .confirmed → ev = .observed)
    (ceil : Kind → Cost) (s : S) (e : Event) (h : Inv s) : Inv (stepWith vf ceil s e) := by
  have hd := desk_ok s.desk e s.sentToday h.pushes h.budget
  refine ⟨cinv_step vf hvf ceil _ s.card e h.card, lessons_step s.card s.lessons e h.lessons, hd.1, hd.2.1,
    by simp [stepWith, h.badPush, hd.2.2], ?_⟩
  · -- no Go asked and no act or one-way phase started in quiet hours
    have hq := h.quietAct
    simp only [stepWith, hq, Bool.false_or]
    cases hqq : quiet s.desk.clock
    · simp
    · simp only [Bool.true_and]
      cases e with
      | ask n => simp [actStarted, cardStep]
      | dispatch n =>
          simp only [actStarted, cardStep]
          split
          · rename_i p hp
            split
            · rename_i hc
              simp only [Bool.and_eq_true, Bool.not_eq_true', Bool.true_and] at hc
              simp [hp, hc.2]
            · cases hr : s.card.running <;> simp
          · cases hr : s.card.running <;> simp
      | _ => simp [actStarted]

theorem inv_run (vf : Evidence → Verdict) (hvf : ∀ ev, vf ev = .confirmed → ev = .observed)
    (ceil : Kind → Cost) (evs : List Event) : ∀ s : S, Inv s → Inv (evs.foldl (stepWith vf ceil) s) := by
  induction evs with
  | nil => intro s h; exact h
  | cons e rest ih => intro s h; exact ih _ (inv_step vf hvf ceil s e h)

theorem inv_start (p : Plan) : Inv (start p) := by
  refine ⟨⟨?_, ?_, by simp [start], fun _ => ⟨rfl, rfl⟩, by simp [start], by simp [start]⟩,
    by simp [start], rfl, by simp [start, PUSH_BUDGET], rfl, rfl⟩
  · intro x hx
    simp only [start, List.mem_map] at hx
    obtain ⟨_, _, rfl⟩ := hx
    exact ⟨by simp, by simp, by simp, by simp, by simp, by simp, by simp [MAX_RETRIES], by simp, by simp, by simp,
      rfl, rfl⟩
  · intro x hx
    simp only [start, List.mem_replicate] at hx
    rw [hx.2]; simp

theorem spec_of_inv (s : S) (h : Inv s) : Spec s = true := by
  have hc := h.card
  simp only [Spec, DoneNeedsEvidence, OneYesOneAct, BudgetBeforeDispatch, PushBudgetQuiet,
    NoActReplayAfterRestart, OneWayNeverAutoRetried, LessonsBounded, Bool.and_eq_true, List.all_eq_true,
    Bool.or_eq_true, bne_iff_ne, ne_eq, decide_eq_true_eq, Bool.not_eq_true', beq_iff_eq, h.badPush,
    h.quietAct, h.pushes]
  refine ⟨⟨⟨⟨⟨⟨?_, ?_⟩, ⟨hc.usd, hc.secs⟩⟩, ⟨⟨⟨trivial, trivial⟩, h.budget⟩, trivial⟩⟩, ?_⟩, ?_⟩, ?_⟩
  · by_cases hd : s.card.status = .done
    · rcases hc.done hd with hb | ⟨hne, hall⟩
      · exact Or.inl (Or.inr hb)
      · exact Or.inr ⟨by simpa using hne, hall⟩
    · exact Or.inl (Or.inl hd)
  · intro p hp; have := hc.phases p hp; exact ⟨this.cnt, this.stale⟩
  · intro p hp; exact (hc.phases p hp).replay
  · intro p hp
    have hi := hc.phases p hp
    refine ⟨?_, hi.retrMax⟩
    by_cases hd : p.door = .oneWay
    · have ⟨ha, hr⟩ := hi.ow hd
      have := hi.cnt
      exact Or.inr ⟨by omega, hr⟩
    · exact Or.inl hd
  · intro k _; exact h.lessons k

theorem verdictOf_sound : ∀ ev, verdictOf ev = .confirmed → ev = .observed := by
  intro ev; cases ev <;> simp [verdictOf]

/-- For every plan take_on can make, every ceiling table and every trace of the owner's taps and words, the
executors' results, the oracles, restarts, reflections, the clock and the Desk: the card is done only on
evidence or the owner's word; each one-way dispatch spends its own yes, given at the current revision; the
budget holds before each dispatch; interrupting messages keep quiet hours, breakpoints and the daily budget;
no act in flight at a restart is run again without a new yes; a one-way act is never retried by itself and a
two-way phase at most twice; at most three lessons per key. -/
theorem code_invariant (ceil : Kind → Cost) (p : Plan) (evs : List Event) : Spec (run ceil p evs) = true :=
  spec_of_inv _ (inv_run verdictOf verdictOf_sound ceil evs (start p) (inv_start p))

/-! ## The positive control and worked traces -/

/-- An oracle that takes the executor's sentence for evidence. -/
def naiveVerdict : Evidence → Verdict
  | .observed => .confirmed
  | .executorText => .confirmed
  | .contradicted => .failed
  | .missing => .unverifiable

def ceil0 : Kind → Cost := fun _ => ⟨10, 600⟩

/-- A desk to buy (design 4.1): research and assess are two-way, the order is one-way; three checks. -/
def orderDesk : Plan :=
  { phases := [(.research, .twoWay), (.assess, .twoWay), (.act, .oneWay)], checks := 3, cap := ⟨100, 1800⟩,
    mode := .shadow }

/-- The positive control: with the naive oracle, a research-only card whose executor said "done" closes as done
with no evidence. -/
theorem naive_violates :
    let s := [.clock 600, .dispatch 0, .result 0 true ⟨3, 60⟩, .oracle 0 .executorText, .close].foldl
      (stepWith naiveVerdict ceil0) (start { orderDesk with phases := [(.research, .twoWay)], checks := 1 })
    s.card.status = .done ∧ DoneNeedsEvidence s = false := by
  decide +kernel

/-- The same trace with the code's oracle: unverified, not done. -/
theorem executor_word_is_unverified :
    (run ceil0 { orderDesk with phases := [(.research, .twoWay)], checks := 1 }
      [.clock 600, .dispatch 0, .result 0 true ⟨3, 60⟩, .oracle 0 .executorText, .close]).card.status = .unverified := by
  decide +kernel

/-- The whole job of design 4.1 closes as done on three observed checks, with one yes for one act. -/
theorem desk_job_closes :
    let s := run ceil0 orderDesk [.clock 600, .dispatch 0, .result 0 true ⟨3, 90⟩, .oracle 0 .observed,
      .dispatch 1, .result 1 true ⟨2, 20⟩, .oracle 1 .observed, .ask 2, .answer 2 .yes, .dispatch 2,
      .result 2 true ⟨0, 500⟩, .oracle 2 .observed, .close]
    s.card.status = .done ∧ (s.card.phases.map (·.attempts)) = [1, 1, 1] ∧ s.card.spent = ⟨5, 610⟩ := by
  decide +kernel

/-- Routine two-way acts dispatch without a preliminary Go; dangerous one-way acts still wait for one.
    This mirrors chief_card._phase/_phase_from after removing the obsolete cannot-ask executor floor. -/
theorem routine_act_runs_dangerous_act_waits :
    let routine := { orderDesk with phases := [(.act, .twoWay)], checks := 1 }
    (run ceil0 routine [.clock 600, .dispatch 0]).card.running = some 0 ∧
    (run ceil0 { routine with phases := [(.act, .oneWay)] }
      [.clock 600, .dispatch 0]).card.running = none := by
  decide +kernel

/-- A Go that times out approves nothing: the act does not start. -/
theorem timeout_is_no :
    (run ceil0 orderDesk [.clock 600, .ask 2, .answer 2 .timeout, .dispatch 2]).card.running = none := by
  decide +kernel

/-- A revision between the yes and the dispatch cancels the yes. -/
theorem revision_cancels_the_yes :
    (run ceil0 orderDesk [.clock 600, .ask 2, .answer 2 .yes, .revise, .dispatch 2]).card.running = none := by
  decide +kernel

/-- The act was in flight at a restart: "It didn't" does not run it again; a new Go and yes does. -/
theorem restart_needs_a_new_yes :
    let pre : List Event := [.clock 600, .ask 2, .answer 2 .yes, .dispatch 2, .restart ⟨0, 30⟩, .didnt 2, .dispatch 2]
    (run ceil0 orderDesk pre).card.running = none ∧
    (run ceil0 orderDesk (pre ++ [.ask 2, .answer 2 .yes, .dispatch 2])).card.running = some 2 := by
  decide +kernel

/-- A failed one-way act is never retried by itself, even when the reflection says retry. -/
theorem one_way_failure_waits_for_a_go :
    (run ceil0 orderDesk [.clock 600, .ask 2, .answer 2 .yes, .dispatch 2, .result 2 false ⟨0, 30⟩,
      .reflect 2 7 true, .dispatch 2]).card.running = none := by
  decide +kernel

/-- A two-way phase retries itself twice (three trials), then stops; lessons stay at three for its key. -/
theorem two_way_three_trials :
    let s := run (fun _ => ⟨1, 10⟩) orderDesk [.clock 600,
      .dispatch 0, .result 0 false ⟨1, 10⟩, .reflect 0 5 true,
      .dispatch 0, .result 0 false ⟨1, 10⟩, .reflect 0 5 true,
      .dispatch 0, .result 0 false ⟨1, 10⟩, .reflect 0 5 true,
      .dispatch 0, .reflect 0 5 true]
    (s.card.phases.map (·.attempts)).head? = some 3 ∧ s.card.running = none ∧ s.lessons = [5, 5, 5] := by
  decide +kernel

/-- Pushes on, 10:00, at a breakpoint: the third unprompted message of the day is batched; a restart keeps the
count; at 23:00 a reminder waits. -/
theorem third_push_is_batched :
    let s := run ceil0 { orderDesk with mode := .on } [.clock 600, .breakpoint, .offer .unprompted, .offer .unprompted,
      .restart ⟨0, 0⟩, .breakpoint, .offer .unprompted]
    s.desk.route = .batch ∧ s.desk.pushes = 2 ∧
    (run ceil0 orderDesk [.clock 1380, .breakpoint, .offer .reminder]).desk.route = .batch := by
  decide +kernel

/-- An asked reminder with no breakpoint goes once it has waited 45 minutes (owner, 2026-10-06: at 19:45, not
the next morning's brief); before it has waited it is held, and in quiet hours waiting is not enough. -/
theorem waited_reminder_goes :
    (run ceil0 orderDesk [.clock 1185, .waited, .offer .reminder]).desk.route = .now ∧
    (run ceil0 orderDesk [.clock 1140, .offer .reminder]).desk.route = .batch ∧
    (run ceil0 orderDesk [.clock 1380, .waited, .offer .reminder]).desk.route = .batch := by
  decide +kernel

/-- An act is not started in quiet hours, even with its yes. -/
theorem no_act_at_night :
    (run ceil0 orderDesk [.clock 600, .ask 2, .answer 2 .yes, .clock 1380, .dispatch 2]).card.running = none := by
  decide +kernel

end Eve.Chief
