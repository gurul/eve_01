/-
  Eve.LinkTurn — whose press is live on an Eve Link call (the stick's or the phone's), and the one automatic
  retry after eve's address changed.

  The stick was removed on 2026-10-07 (chore/remove-stick): Relay.swift has no `fromStick`, `setStick` or
  `endStickTurn`, and `TurnOwner` is `.none | .phone`. That code is `Ver.fix` with no stick events, so its traces
  are a subset of the traces proved here: P2-P4 hold for it as they are, and P1 holds trivially (one source). The
  model is kept as the record of the findings; the line numbers below are those of the commits named.

  Models (line numbers are those of the files at 2ad3550):
  - ios/EveLink/Shared/PhoneTurn.swift: `TurnOwner` (42-49).
  - ios/EveLink/Shared/Relay.swift:
    - `turn`, `batch`, `replyToPhone`, `holdDown`, `lastEndsItself`, `retriedAddress` (71-82);
    - `setStick(false)` (104-116) and `refreshAddress` (120-135);
    - `fromStick`: `.talk` (169-194), `.audio` (195-204), `.done` (205-217), `.disconnected` (229-234);
    - `fromEve`: `.open` (245-248) and `.ended` with its address retry (278-307);
    - the hang-up timer `armHangUp` (314-326);
    - `press` (340-350), `holdBegan` (353-362), `holdEnded` (365-369), `endPhoneCall` (372-380);
    - `phoneBegan` (382-431), `fromPhoneMic` (433-451), `phoneEnded` (453-463), `phoneLost` (467-472),
      `stopPhone` (474-482).
  - ios/EveLink/Shared/CallSocket.swift: `send` queues until the call is up (62-64); `hangUp` and `close`
    (57-61, 121-129). `close` drops the queue and sends `.ended` only when it has a reason, so a hang-up
    sends no `.ended`.
  - ios/EveLink/App/EveLinkApp.swift: the Hold to talk button's `holding`, `release` and `holdBegan`
    (120-127, 159-178).
  - ios/EveLink/App/PhoneLink.swift:16: a `refreshAddress` at app start.
  - The daemon's side of a press, bridge/src/eve_bridge/phone_call.py: `_begin_press` (615-621) starts a
    press only when none is open, `_feed` (637) drops frames outside a press, and `done` (726-731) closes it.
    A new call starts with no press.

  The properties, over every trace of events:
  - (P1) frames from the stick and from the phone never go into the same press. Every `done` closes frames from
    one source only, and so does a hang-up.
  - (P2) between two successful opens of the call (`.open`), at most one automatic address retry starts.
  - (P3) the retry restarts a turn that a finger held only while the finger is still on the Hold to talk button.
  - (P4) the retry never starts its turn after a newer turn began, or the call opened, while it awaited
    `refreshAddress()` (ghost `clobber`).

  Four versions are modelled:
  - The code at 2ad3550 (`Ver.cur`), on which the findings were proved; the line numbers above are its. Its
    address retry and `setStick` came in 1c68787.
  - The fixed code (`Ver.fix`, branch fix/lean-findings, 2026-10-04). Its lines:
    - PhoneTurn.swift: `TurnOwner.stickOffMustEndStickTurn` (52), true only for `.stick`; `struct TurnHold`
      (57-73) holds `holdDown` and `lastEndsItself`: `began(endsItself:)`, `pressed()`, `tookOverLiveTurn()`
      (`lastEndsItself = false`), `failed()` and `released()` (`holdDown = false`), and
      `retryRestarts(endsItself:)` = `endsItself || holdDown`.
    - Relay.swift: `hold = TurnHold()` (80); `setStick(false)` (111-118) calls `endStickTurn()` (442-450: the
      batch, then `call.done()`), then `turn = .none` and `armHangUp()`; the retry captures
      `hold.lastEndsItself` (291) and guards on `hold.retryRestarts` (298); `holdBegan` (359-375) calls
      `hold.pressed()` (361), `hold.tookOverLiveTurn()` when it takes over a live turn (366), and `hold.failed()`
      on any throw (372); `holdEnded` calls `hold.released()` (380); `phoneBegan` calls `hold.began` (398) and
      sends a live stick press through the same `endStickTurn()` (421-425).
  - 1dd0f26 (`Ver.mid`), the first fix: P1-P3 as `Ver.fix` below, but the retry's `Task` checked only
    `hold.retryRestarts` (Relay.swift:298 there) after awaiting `refreshAddress()`.
  - The P4 fix (`Ver.fix` again, branch fix/lean-findings after 1dd0f26). Its lines:
    - PhoneTurn.swift:79-90 `struct TurnGeneration`: `bump()` counts turns begun and opens;
      `retryMayStart(since:turn:callOpen:)` = `since == count && turn == .none && !callOpen`.
    - Relay.swift: `generation` (82); `generation.bump()` at `turn = .stick` in `.talk` (196), at `.open` (256) and
      at `turn = .phone` in `phoneBegan` (441); the retry captures `since = generation.count` (295) and, after
      `retryRestarts`, guards on `generation.retryMayStart(since:turn:callOpen:)` (305-309). `refreshAddress`
      has hung up by then, so `callOpen` is false there and the model reads only `turn == .none` and the count.
  - 48df362, which added the phone's turns (`Ver.old`). It already had one `TurnOwner`, not separate flags, with
    the same `fromStick`, `phoneBegan` and `phoneEnded` as now. It had no `setStick`, no `refreshAddress` and no
    retry. `old_p1_invariant` proves P1 of it for every trace: that version did not mix sources. A phone press
    during a stick press sends the stick's press first (`phoneMustEndStickTurn`, Relay.swift:408-417 at 2ad3550). The
    stick's press during a phone turn does the same (173-177). No version before 48df362 had a phone turn.

  Findings, in the code at 2ad3550 (`current_violates`), all three FIXED as `Ver.fix` models (`fixed_invariant`):
  - P1. `setStick(false)` (Relay.swift:112) ends a live stick press with `turn = .none; batch.removeAll()`. It
    sends no `done` and does not hang up, so the daemon's press stays open with the stick's frames in it. The
    phone's next turn is then sent into that press. `phoneBegan` sees `turn == .none`, so it sends no `done`
    first, and the daemon ignores its `talk`, because a press is open. The question eve hears is the stick's
    words and the phone's words together. Trace `p1Trace`. Fix (Relay.swift:111-118): send the stick's press first,
    as `phoneBegan` does, through `endStickTurn()` (the batch, then `call.done()`), before `turn = .none`. The
    fix also arms the hang-up timer; the model lets that timer fire at any moment the turn is `.none` already.
  - P3(a). `holdBegan` on a live Action Button turn (357-359) makes that turn held: `endOfSpeech` becomes held.
    It leaves `lastEndsItself == true`. If the address then changes, the retry captures `endsItself = true`
    (285), and its guard `endsItself || self.holdDown` (292) passes after the finger has lifted. The turn
    restarts with the finger up: the microphone opens again after the owner let go. That restarted turn ends
    itself (a 1.2 s pause, or 8 s with no speech). Trace `p3TraceA`. Fix: `hold.tookOverLiveTurn()`
    (Relay.swift:366) sets `lastEndsItself = false` in that branch.
  - P3(b). `holdBegan` sets `holdDown = true` (355) before anything can throw. When `phoneBegan` throws (the
    microphone did not start, say), the app sets `holding = false` (EveLinkApp.swift:177), so lifting the
    finger never calls `holdEnded`, and `holdDown` stays true with no finger. A retry still waiting for a held
    turn then passes its guard on that stale `holdDown` and starts a held turn (`EndOfSpeech.held()`) that no
    finger holds. The microphone stays open, and the room is sent as a question, until the 60 s cap. Trace
    `p3TraceB`. Fix: `holdBegan`'s body runs in a `do/catch` whose `catch` calls `hold.failed()`
    (`holdDown = false`) and rethrows (Relay.swift:362-374). The model's throwing hold is the one where
    `phoneBegan` throws; a "not paired" throw is outside it (paired throughout), and the fix clears `holdDown`
    there too.
  - P4, at 1dd0f26 (`current_retry_clobbers`). The retry's `Task` awaits `refreshAddress()`; the owner may start
    a new turn meanwhile (the stick, the Action Button, Hold to talk) and finish it. When the refresh returns, the
    guard `retryRestarts` passes for a turn that ends itself, and `phoneBegan` starts the LOST turn again: the
    microphone opens after the owner already moved on. Trace `p4Trace`. Fix: `TurnGeneration` (above).
  - P2 held at 2ad3550 already (`current_p2_invariant`), and the fix leaves it as it was. `retriedAddress` is set when a retry starts (284) and
    is cleared only by `.open` (247), so a second address error before an open does not retry again.

  Abstractions, stated:
  - Paired throughout, and `phone` is present. A press or a retry's `phoneBegan` starts its turn. `holdBegin`
    carries whether its `phoneBegan` throws (`fails`). A press that throws starts no turn and touches nothing
    the properties read, so it is not an event.
  - `phoneLive` is folded into `replyToPhone`. `phoneBegan` sets both, and `stopPhone` clears both, so
    `phoneLive → replyToPhone`, and the `phoneLive || replyToPhone` test of `.talk` (170) is `reply`.
    `phoneLost` (467-472) is `phoneEnded` when the phone is talking.
  - The batch is shared by both sources. `batch` is the source of the samples in it. Appending samples of
    another source counts as a mix, which is stricter than the code needs.
  - `full` on an audio event: the batch reached `batchSamples` and goes out as one frame.
  - The wire: `dTalking`/`dSrc` are the daemon's open press and the source of its frames. Messages sent while
    the call is not up are queued and delivered in order on the next open (CallSocket.swift:62-64), so they
    count toward the next call. A hang-up or an `.ended` while a call exists starts the next call with no press.
  - Stick events may arrive at any time, even with the stick switched off: more traces than the code allows.
    The hang-up timer (316-325) is modelled without its `queuedAudioFrames == 0` test: more hang-ups.
  - The retry's `Task` (287-298) is a waiting flag per kind: (endsItself, the lost turn was held), the second
    a ghost. `retryOk` is that `Task` resuming with `refreshAddress() == true`. That function hangs up first
    (132), then the guard (292) and `phoneBegan` run. A flag stays set once its task resumed, so the model lets a
    waiting retry resume more than once. A refresh that returns false starts nothing and is not an event.
    `addrMoved` is the app-start refresh (PhoneLink.swift:16) finding a new address: a hang-up.
  - P4's retries (`Fresh`) are kept per kind, as the waiting flags are: a newer retry of the same kind replaces the
    older in the model. The older one's `Task` can then only find its count stale; it hangs up in
    `refreshAddress` and starts nothing, which is `addrMoved`. `generation.count` is abstracted to one Bool per
    kind, "the count is still the one this retry captured". The ghost (`g*`) reads the press: a turn began
    (`turn` became `.stick` or `.phone` from something else) or the call opened (`sock` became true). `.wake`
    (the stick picked up opens the call) is not an event of this model.
  - `finger` is a ghost: the touch on the Hold to talk button, as the app sees it. A trip to the background is
    a lift (EveLinkApp.swift:131). `holdBegin` is the touch going down and the app's `holdBegan` running.
    The first-ever hold, which only asks for the microphone, touches nothing modelled.
-/
namespace Eve.LinkTurn

/-- `TurnOwner` (PhoneTurn.swift:42-49), reused for the source of the batch and of the daemon's press. -/
inductive Owner where
  | none | stick | phone
  deriving DecidableEq, Repr

instance (p : Owner → Prop) [DecidablePred p] : Decidable (∀ o, p o) :=
  if h : p .none ∧ p .stick ∧ p .phone then
    isTrue (fun o => by cases o <;> simp_all)
  else isFalse (fun H => h ⟨H _, H _, H _⟩)

/-- Which version of Relay.swift. -/
inductive Ver where
  | old   -- 48df362
  | cur   -- 2ad3550, where the findings were proved
  | mid   -- 1dd0f26: P1-P3 fixed, the retry not guarded against a newer turn (P4)
  | fix   -- the fixed code (fix/lean-findings, 2026-10-04)
  deriving DecidableEq, Repr

inductive Event where
  | stickTalk                       -- fromStick(.talk)
  | stickAudio (full : Bool)        -- fromStick(.audio)
  | stickDone                       -- fromStick(.done)
  | stickGone                       -- fromStick(.disconnected)
  | stickOff                        -- setStick(false)
  | press                           -- press(): the Action Button, the island's button, evelink://talk
  | holdBegin (fails : Bool)        -- a finger on Hold to talk: the app's holdBegan → relay.holdBegan()
  | holdEnd                         -- the finger lifts: the app's release() → relay.holdEnded() if `holding`
  | phoneAudio (full : Bool)        -- fromPhoneMic, EndOfSpeech says .going
  | phoneSpoke                      -- fromPhoneMic, .spoke or .tooLong → phoneEnded
  | phoneSilent                     -- fromPhoneMic, .silent → endPhoneCall
  | phoneLost                       -- PhoneAudio.onLost → phoneLost
  | endCall                         -- the End call button → endPhoneCall
  | opened                          -- fromEve(.open)
  | endedAddr                       -- fromEve(.ended) whose reason says "address changed"
  | endedOther                      -- fromEve(.ended), any other reason
  | hangUpTimer                     -- armHangUp's timer fires
  | addrMoved                       -- refreshAddress() from app start finds a new address: call.hangUp()
  | retryOk (ends held : Bool)      -- a waiting retry's refreshAddress() returned true
  deriving DecidableEq, Repr

instance (p : Event → Prop) [DecidablePred p] : Decidable (∀ e, p e) :=
  if h : p .stickTalk ∧ p (.stickAudio true) ∧ p (.stickAudio false) ∧ p .stickDone ∧ p .stickGone ∧
      p .stickOff ∧ p .press ∧ p (.holdBegin true) ∧ p (.holdBegin false) ∧ p .holdEnd ∧
      p (.phoneAudio true) ∧ p (.phoneAudio false) ∧ p .phoneSpoke ∧ p .phoneSilent ∧ p .phoneLost ∧
      p .endCall ∧ p .opened ∧ p .endedAddr ∧ p .endedOther ∧ p .hangUpTimer ∧ p .addrMoved ∧
      p (.retryOk true true) ∧ p (.retryOk true false) ∧ p (.retryOk false true) ∧ p (.retryOk false false) then
    isTrue (fun e => by
      obtain ⟨h1, h2, h3, h4, h5, h6, h7, h8, h9, h10, h11, h12, h13, h14, h15, h16, h17, h18, h19, h20, h21,
        h22, h23, h24, h25⟩ := h
      cases e with
      | stickAudio b => cases b <;> assumption
      | holdBegin b => cases b <;> assumption
      | phoneAudio b => cases b <;> assumption
      | retryOk a b => cases a <;> cases b <;> assumption
      | _ => assumption)
  else isFalse (fun H => h ⟨H _, H _, H _, H _, H _, H _, H _, H _, H _, H _, H _, H _, H _, H _, H _, H _, H _,
    H _, H _, H _, H _, H _, H _, H _, H _⟩)

/-! ## The press: who owns it, and what reaches the daemon (P1) -/

structure Link where
  turn : Owner := .none        -- Relay.turn
  batch : Owner := .none       -- the source of the samples in Relay.batch (.none: empty)
  dSrc : Owner := .none        -- ghost: the source of the frames in the daemon's open press (.none: none yet)
  dTalking : Bool := false     -- ghost: the daemon has a press open (Call.talking)
  mixed : Bool := false        -- ghost: one press got frames from both sources
  sock : Bool := false         -- CallSocket.isOpen
  reply : Bool := false        -- Relay.replyToPhone (and phoneLive, folded in)
  deriving DecidableEq, Repr

namespace Link

/-- `call.audio(frame)`: the daemon adds it to its open press, or drops it (`_feed`, phone_call.py:637). -/
def frames (l : Link) (src : Owner) : Link :=
  if l.dTalking then { l with mixed := l.mixed || (l.dSrc != .none && l.dSrc != src), dSrc := src } else l

/-- `if !batch.isEmpty { call.audio(batch) }`, and the batch is emptied. -/
def flush (l : Link) : Link :=
  if l.batch == .none then l else { l.frames l.batch with batch := .none }

/-- `call.done()`: the daemon closes the press (phone_call.py:726-731). -/
def done (l : Link) : Link := { l with dTalking := false, dSrc := .none }

/-- `call.talk()`: the daemon opens a press only when none is open (`_begin_press`, phone_call.py:615-621). -/
def talk (l : Link) : Link := if l.dTalking then l else { l with dTalking := true, dSrc := .none }

/-- `call.hangUp()` (CallSocket.swift:57-61), and the close behind `.ended`: only with a call; the next call
starts with no press. -/
def close (l : Link) : Link :=
  if l.sock then { l with sock := false, dTalking := false, dSrc := .none } else l

/-- `if !call.isOpen { call.open(...) }`. -/
def openSock (l : Link) : Link := { l with sock := true }

/-- `batch += pcm`, then one frame out when the batch is full. -/
def append (l : Link) (src : Owner) (full : Bool) : Link :=
  let l := { l with mixed := l.mixed || (l.batch != .none && l.batch != src), batch := src }
  if full then l.flush else l

/-- `stopPhone` (Relay.swift:474-482). -/
def stopPhone (l : Link) : Link := { l with reply := false }

/-- `phoneBegan` (Relay.swift:382-431), from line 408: a live stick press goes as its own press first (408-417),
then `turn = .phone`, `replyToPhone = true`, `batch.removeAll()`, open if needed, `call.talk()`. -/
def phoneBegan (l : Link) : Link :=
  let l := if l.turn == .stick then l.flush.done else l
  ({ l with turn := .phone, reply := true, batch := .none } : Link).openSock.talk

/-- `phoneEnded` (Relay.swift:453-463): `guard phoneTalking`, then the batch and `call.done()`. -/
def phoneEnded (l : Link) : Link :=
  if l.turn == .phone then ({ l with turn := .none } : Link).flush.done else l

/-- `endPhoneCall` (Relay.swift:372-380). -/
def endPhoneCall (l : Link) : Link := ({ l with turn := .none, batch := .none } : Link).close.stopPhone

/-- One event. `gate` is what the rest of the app decides for this event: for `holdEnd`, whether the app calls
`holdEnded` (its `holding`); for `retryOk`, whether the retry's guard lets `phoneBegan` run. The invariant is
proved for every `gate`. -/
def step (v : Ver) (l : Link) (e : Event) (gate : Bool) : Link :=
  match e with
  -- Relay.swift:169-194. `if phoneLive || replyToPhone`: a live phone turn goes as its own press (173-177), then
  -- stopPhone. Then `turn = .stick`, `batch.removeAll()`, open if needed, `call.talk()`.
  | .stickTalk =>
      let l := if l.reply then
          (if l.turn == .phone then ({ l.flush.done with turn := .none } : Link) else l).stopPhone
        else l
      ({ l with turn := .stick, batch := .none } : Link).openSock.talk
  -- 195-204: `guard turn.takesStickAudio`.
  | .stickAudio full => if l.turn == .stick then l.append .stick full else l
  -- 205-217: `guard turn.takesStickAudio`, `turn = .none`, the batch, `call.done()`.
  | .stickDone => if l.turn == .stick then ({ l with turn := .none } : Link).flush.done else l
  -- 229-234: `guard !replyToPhone`, `turn = .none`, `call.hangUp()`.
  | .stickGone => if l.reply then l else ({ l with turn := .none } : Link).close
  -- 104-116. 48df362 had no stick switch. 2ad3550 (112): `if turn == .stick { turn = .none; batch.removeAll() }`,
  -- with no `done`. Fixed (111-118): `endStickTurn()` (442-450: the batch, `call.done()`), then `turn = .none`.
  | .stickOff =>
      match v with
      | .old => l
      | .cur => if l.turn == .stick then { l with turn := .none, batch := .none } else l
      | .mid | .fix => if l.turn == .stick then ({ l with turn := .none } : Link).flush.done else l
  -- 340-350: `.send` when the phone is talking, else `.start`/`.bargeIn` → phoneBegan(endsItself: true).
  | .press => if l.turn == .phone then l.phoneEnded else l.phoneBegan
  -- 353-362: a phone turn already live is made held (no change here); else phoneBegan(endsItself: false).
  | .holdBegin fails => if l.turn == .phone then l else if fails then l else l.phoneBegan
  -- 365-369, reached only when the app's `holding` is true (EveLinkApp.swift:159-162).
  | .holdEnd => if gate then l.phoneEnded else l
  -- 433-439: `guard phoneTalking`.
  | .phoneAudio full => if l.turn == .phone then l.append .phone full else l
  -- 445
  | .phoneSpoke => l.phoneEnded
  -- 446-449
  | .phoneSilent => if l.turn == .phone then l.endPhoneCall else l
  -- 467-472
  | .phoneLost => l.phoneEnded
  -- 372-380
  | .endCall => l.endPhoneCall
  -- 245-248
  | .opened => l
  -- 278-307: only a call that exists ends (CallSocket.close); `turn = .none`, `stopPhone()`. The batch stays.
  | .endedAddr | .endedOther =>
      if l.sock then ({ l.close with turn := .none } : Link).stopPhone else l
  -- 316-325: `guard !self.talking`, then `call.hangUp()` and `stopPhone()`.
  | .hangUpTimer => if l.turn == .none then l.close.stopPhone else l
  -- PhoneLink.swift:16 → refreshAddress (131-132): a new address hangs up.
  | .addrMoved => match v with
      | .old => l
      | _ => l.close
  -- 287-298: refreshAddress hung up (132); then, if the guard (292) passes, phoneBegan.
  | .retryOk _ _ => match v with
      | .old => l
      | _ => if gate then l.close.phoneBegan else l.close

/-- P1 and what keeps it: a press is open only on a call (a `talk` always follows an open); the daemon's press
holds only the owner's frames; a closed press has no source; a non-empty batch belongs to the owner, or to a turn
that is over; a phone turn replies on the phone. -/
def inv (l : Link) : Bool :=
  !l.mixed &&
  (l.sock || !l.dTalking) &&
  (l.dTalking || l.dSrc == .none) &&
  (l.dSrc == .none || l.dSrc == l.turn) &&
  (l.batch == .none || l.turn == .none || l.batch == l.turn) &&
  (l.turn != .phone || l.reply)

end Link

/-- One step of 48df362 (and of the fix, below) keeps P1's invariant, for every state, event and `gate`. -/
theorem link_step_old : ∀ (e : Event) (g : Bool) (t b d : Owner) (dt mx sk rp : Bool),
    Link.inv ⟨t, b, d, dt, mx, sk, rp⟩ = true → Link.inv (Link.step .old ⟨t, b, d, dt, mx, sk, rp⟩ e g) = true := by
  decide +kernel

theorem link_step_fix : ∀ (e : Event) (g : Bool) (t b d : Owner) (dt mx sk rp : Bool),
    Link.inv ⟨t, b, d, dt, mx, sk, rp⟩ = true → Link.inv (Link.step .fix ⟨t, b, d, dt, mx, sk, rp⟩ e g) = true := by
  decide +kernel

/-! ## The retry count (P2) -/

/-- Retries started since the last `.open`, saturating. -/
inductive Count where
  | zero | one | many
  deriving DecidableEq, Repr

def Count.bump : Count → Count
  | .zero => .one
  | _ => .many

instance (p : Count → Prop) [DecidablePred p] : Decidable (∀ c, p c) :=
  if h : p .zero ∧ p .one ∧ p .many then
    isTrue (fun c => by cases c <;> simp_all)
  else isFalse (fun H => h ⟨H _, H _, H _⟩)

/-- What the retry and the hold read from the press. -/
structure View where
  phoneTurn : Bool   -- `turn == .phone` (phoneTalking)
  sock : Bool        -- a call exists
  fires : Bool       -- an address `.ended` now starts a retry: a call, a phone turn, `!retriedAddress` (282)
  fresh : Bool       -- for `retryOk`: the P4 guard passes (`generation.retryMayStart`, fixed code only)

structure Retry where
  retried : Bool := false      -- Relay.retriedAddress
  retries : Count := .zero     -- ghost
  deriving DecidableEq, Repr

namespace Retry

def step (r : Retry) (w : View) : Event → Retry
  -- 282-284: `reason.contains("address changed"), phoneTurnLost, !retriedAddress` → `retriedAddress = true`.
  | .endedAddr => if w.fires then { retried := true, retries := r.retries.bump } else r
  -- 245-247: `.open` clears `retriedAddress`.
  | .opened => if w.sock then { retried := false, retries := .zero } else r
  | _ => r

def inv (r : Retry) : Bool := r.retries != .many && (r.retried || r.retries == .zero)

end Retry

/-- One step keeps P2's invariant, given that a retry fires only when `retriedAddress` is false. -/
theorem retry_step_inv : ∀ (e : Event) (pt sk f fr rt : Bool) (c : Count),
    Retry.inv ⟨rt, c⟩ = true → (f = true → rt = false) → Retry.inv (Retry.step ⟨rt, c⟩ ⟨pt, sk, f, fr⟩ e) = true := by
  decide +kernel

/-! ## The finger and the waiting retries (P3) -/

structure Hold where
  holdDown : Bool := false     -- Relay.holdDown
  finger : Bool := false       -- ghost: the touch on Hold to talk
  holding : Bool := false      -- EveLinkApp.holding
  heldTurn : Bool := false     -- ghost: the phone's turn is held by a finger (EndOfSpeech.held, or made held)
  lastEnds : Bool := true      -- Relay.lastEndsItself
  pTT : Bool := false          -- a retry waits that captured endsItself = true for a held turn
  pFT : Bool := false          -- a retry waits that captured endsItself = false for a held turn
  pFree : Bool := false        -- a retry waits for a turn no finger held
  bad : Bool := false          -- ghost: a retry restarted a held turn with the finger up
  deriving DecidableEq, Repr

namespace Hold

/-- A retry starts (282-286): it captures `endsItself = lastEndsItself` (285), and the ghost whether the lost
turn was held. -/
def wait (h : Hold) (ends held : Bool) : Hold :=
  if held then (if ends then { h with pTT := true } else { h with pFT := true }) else { h with pFree := true }

/-- A waiting retry of this kind resumes with a new address: the guard `endsItself || self.holdDown` (292). -/
def restarts (h : Hold) (ends held : Bool) : Bool :=
  (if held then (if ends then h.pTT else h.pFT) else h.pFree) && (ends || h.holdDown)

def step (v : Ver) (h : Hold) (w : View) : Event → Hold
  -- press → phoneBegan(endsItself: true), 385: `lastEndsItself = true`; the turn ends itself.
  | .press => if w.phoneTurn then h else { h with lastEnds := true, heldTurn := false }
  -- EveLinkApp.swift:123 `holding = true`, then Relay.swift:353-362: `holdDown = true` first (355).
  | .holdBegin fails =>
      let h := { h with finger := true, holdDown := true }
      if w.phoneTurn then
        -- 357-359: the live turn is made held. 2ad3550 leaves `lastEndsItself` as it was; fixed:
        -- `hold.tookOverLiveTurn()` (366) sets it false.
        { h with holding := true, heldTurn := true, lastEnds := if v == .fix || v == .mid then false else h.lastEnds }
      else if fails then
        -- phoneBegan(endsItself: false) set `lastEndsItself = false` (385) and threw;
        -- EveLinkApp.swift:177 `holding = false`. 2ad3550 leaves `holdDown` true; fixed: the `catch` calls
        -- `hold.failed()` (372), which clears it.
        { h with holding := false, lastEnds := false, holdDown := if v == .fix || v == .mid then false else true }
      else { h with holding := true, lastEnds := false, heldTurn := true }
  -- EveLinkApp.swift:159-162: only a `holding` app calls holdEnded, which sets `holdDown = false` (367).
  | .holdEnd => if h.holding then { h with finger := false, holding := false, holdDown := false }
                else { h with finger := false }
  -- 282-298
  | .endedAddr => if w.fires then h.wait h.lastEnds h.heldTurn else h
  -- 287-298: the guard passes → phoneBegan(endsItself: ends) sets `lastEndsItself = ends` (385). Fixed: and the
  -- P4 guard (305-309), `w.fresh`.
  | .retryOk ends held =>
      if v != .old && h.restarts ends held && w.fresh then
        { h with bad := h.bad || (held && !h.finger), lastEnds := ends, heldTurn := !ends }
      else h
  | _ => h

/-- P3 and what keeps it in the fixed code: `holdDown` only under a finger the app is holding, no retry waits
that would restart a held turn on its own, and a held turn is not one that ends itself. -/
def inv (h : Hold) : Bool :=
  !h.bad && (!h.holdDown || h.finger) && (!h.holdDown || h.holding) && (!h.holding || h.finger) &&
  !h.pTT && (!h.heldTurn || !h.lastEnds)

end Hold

/-- One step of the fixed hold logic keeps P3's invariant, for every state, event and view of the press. -/
theorem hold_step_fix : ∀ (e : Event) (pt sk f fr a b c d i j k l m : Bool),
    Hold.inv ⟨a, b, c, d, i, j, k, l, m⟩ = true →
      Hold.inv (Hold.step .fix ⟨a, b, c, d, i, j, k, l, m⟩ ⟨pt, sk, f, fr⟩ e) = true := by
  decide +kernel

/-! ## A retry against a newer turn (P4) -/

/-- The kind of a waiting retry, as `Hold.wait` files it: (endsItself, the lost turn was held). -/
inductive Kind where
  | tt | ft | free
  deriving DecidableEq, Repr

def kindOf (ends held : Bool) : Kind := if held then (if ends then .tt else .ft) else .free

/-- Where the fixed Relay.swift calls `generation.bump()`: `turn = .stick` in `.talk` (196), `.open` (256), and
`turn = .phone` in `phoneBegan` (441), reached by a press that starts a turn, a hold that does not throw, and a
retry whose guards pass (`g`). -/
def bumps (l : Link) (e : Event) (g : Bool) : Bool :=
  match e with
  | .stickTalk => true
  | .press => l.turn != .phone
  | .holdBegin fails => l.turn != .phone && !fails
  | .opened => true
  | .retryOk _ _ => g
  | _ => false

/-- Ghost: on this step a turn began (`turn` became `.stick` or `.phone` from something else) or the call opened. -/
def began (l l' : Link) : Bool := (l'.turn != .none && l'.turn != l.turn) || (l'.sock && !l.sock)

/-- Whenever a turn begins or the call opens, the fixed code has bumped its count. -/
theorem began_bumps : ∀ (e : Event) (g : Bool) (t b d : Owner) (dt mx sk rp : Bool),
    began ⟨t, b, d, dt, mx, sk, rp⟩ (Link.step .fix ⟨t, b, d, dt, mx, sk, rp⟩ e g) = true →
      bumps ⟨t, b, d, dt, mx, sk, rp⟩ e g = true := by
  decide +kernel

structure Fresh where
  cTT : Bool := false      -- the waiting (ends, held) retry's captured `since` is still `generation.count`
  cFT : Bool := false
  cFree : Bool := false
  gTT : Bool := false      -- ghost: no turn began and the call did not open since that retry started
  gFT : Bool := false
  gFree : Bool := false
  clobber : Bool := false  -- ghost: a retry started its turn after a newer turn began (P4 broken)
  deriving DecidableEq, Repr

namespace Fresh

def code (f : Fresh) : Kind → Bool
  | .tt => f.cTT | .ft => f.cFT | .free => f.cFree

def ghost (f : Fresh) : Kind → Bool
  | .tt => f.gTT | .ft => f.gFT | .free => f.gFree

/-- A retry of kind `k` starts (282-295): it captures `since = generation.count`; nothing has begun since. -/
def start (f : Fresh) : Kind → Fresh
  | .tt => { f with cTT := true, gTT := true }
  | .ft => { f with cFT := true, gFT := true }
  | .free => { f with cFree := true, gFree := true }

/-- What an event is to P4: a retry resuming (`retryOk`), an address `.ended` (`endedAddr`), or anything else. -/
inductive Cls where
  | resume | addr | other
  deriving DecidableEq, Repr

def cls : Event → Cls
  | .retryOk _ _ => .resume
  | .endedAddr => .addr
  | _ => .other

/-- The event itself. `k`: the kind a retry starting now files under; `rk`: the kind of a resuming retry;
`restarted`: it starts its turn (`phoneBegan`). -/
def stage1 (f : Fresh) (c : Cls) (fires : Bool) (k rk : Kind) (restarted : Bool) : Fresh :=
  match c with
  | .resume => { f with clobber := f.clobber || (restarted && !f.ghost rk) }
  | .addr => if fires then f.start k else f
  | .other => f

/-- Then the count (`bumps`) and the ghost (`began`), for this step. -/
def stage2 (f : Fresh) (began bumps : Bool) : Fresh :=
  let f := if bumps then { f with cTT := false, cFT := false, cFree := false } else f
  if began then { f with gTT := false, gFT := false, gFree := false } else f

def step (f : Fresh) (e : Event) (fires : Bool) (k rk : Kind) (restarted began bumps : Bool) : Fresh :=
  (f.stage1 (cls e) fires k rk restarted).stage2 began bumps

def inv (f : Fresh) : Bool :=
  !f.clobber && (!f.cTT || f.gTT) && (!f.cFT || f.gFT) && (!f.cFree || f.gFree)

end Fresh

instance (p : Kind → Prop) [DecidablePred p] : Decidable (∀ k, p k) :=
  if h : p .tt ∧ p .ft ∧ p .free then
    isTrue (fun k => by cases k <;> simp_all)
  else isFalse (fun H => h ⟨H _, H _, H _⟩)

instance (p : Fresh.Cls → Prop) [DecidablePred p] : Decidable (∀ c, p c) :=
  if h : p .resume ∧ p .addr ∧ p .other then
    isTrue (fun c => by cases c <;> simp_all)
  else isFalse (fun H => h ⟨H _, H _, H _⟩)

theorem fresh_stage1 : ∀ (c : Fresh.Cls) (fires : Bool) (k rk : Kind) (restarted a b cc d i j m : Bool),
    Fresh.inv ⟨a, b, cc, d, i, j, m⟩ = true → (restarted = true → Fresh.code ⟨a, b, cc, d, i, j, m⟩ rk = true) →
      Fresh.inv (Fresh.stage1 ⟨a, b, cc, d, i, j, m⟩ c fires k rk restarted) = true := by
  decide +kernel

theorem fresh_stage2 : ∀ (began bumps a b c d i j m : Bool),
    Fresh.inv ⟨a, b, c, d, i, j, m⟩ = true → (began = true → bumps = true) →
      Fresh.inv (Fresh.stage2 ⟨a, b, c, d, i, j, m⟩ began bumps) = true := by
  decide +kernel

/-- One step keeps P4's invariant, when every begin is bumped and a retry restarts only on a current count. -/
theorem fresh_step (f : Fresh) (e : Event) (fires : Bool) (k rk : Kind) (restarted began bumps : Bool)
    (h : f.inv = true) (hb : began = true → bumps = true) (hr : restarted = true → f.code rk = true) :
    (f.step e fires k rk restarted began bumps).inv = true := by
  obtain ⟨a, b, c, d, i, j, m⟩ := f
  have h1 := fresh_stage1 (Fresh.cls e) fires k rk restarted a b c d i j m h hr
  unfold Fresh.step
  generalize Fresh.stage1 ⟨a, b, c, d, i, j, m⟩ (Fresh.cls e) fires k rk restarted = f1 at h1 ⊢
  obtain ⟨a', b', c', d', i', j', m'⟩ := f1
  exact fresh_stage2 began bumps a' b' c' d' i' j' m' h1 hb

/-! ## The relay: the four parts on one event list -/

structure S where
  l : Link := {}
  r : Retry := {}
  h : Hold := {}
  f : Fresh := {}
  deriving DecidableEq, Repr

/-- The kind a retry starting now captures (`Hold.wait`), and the kind of a resuming one. -/
def kindNow (s : S) : Kind := kindOf s.h.lastEnds s.h.heldTurn

def resumeKind : Event → Kind
  | .retryOk ends held => kindOf ends held
  | _ => .free

/-- The P4 guard (Relay.swift:305-309): the count this retry captured, and no turn live. Only the fixed code. -/
def fresh (v : Ver) (s : S) (e : Event) : Bool :=
  v != .fix || (s.f.code (resumeKind e) && s.l.turn == .none)

def view (v : Ver) (s : S) (e : Event) : View :=
  { phoneTurn := s.l.turn == .phone, sock := s.l.sock,
    fires := s.l.sock && s.l.turn == .phone && !s.r.retried, fresh := fresh v s e }

/-- What the app decides for the press's `gate`. -/
def gate (v : Ver) (s : S) : Event → Bool
  | .holdEnd => s.h.holding
  | .retryOk ends held => s.h.restarts ends held && fresh v s (.retryOk ends held)
  | _ => false

def step (v : Ver) (s : S) (e : Event) : S :=
  let g := gate v s e
  let l' := s.l.step v e g
  let restarted := match e with
    | .retryOk _ _ => v != .old && g
    | _ => false
  { l := l', r := s.r.step (view v s e) e, h := s.h.step v (view v s e) e,
    f := s.f.step e (view v s e).fires (kindNow s) (resumeKind e) restarted (began s.l l') (bumps s.l e g) }

def run (v : Ver) (evs : List Event) : S := evs.foldl (step v) {}

def Spec (s : S) : Bool := !s.l.mixed && s.r.retries != .many && !s.h.bad && !s.f.clobber

/-! ## The code at 2ad3550 breaks P1 and P3 -/

/-- P1: the stick's press is live, the stick is switched off, then the phone talks. The daemon's press has the
stick's frames and then the phone's. -/
def p1Trace : List Event := [.stickTalk, .stickAudio true, .stickOff, .press, .phoneAudio true]

/-- P3(a): an Action Button turn, then a finger on Hold to talk, then eve's address changes, then the finger
lifts. The retry restarts the turn with no finger. -/
def p3TraceA : List Event := [.press, .holdBegin false, .endedAddr, .holdEnd, .retryOk true true]

/-- P3(b): a held turn; eve's address changes; the finger lifts; a second hold fails to start the microphone;
that finger lifts. The waiting retry sees the stale `holdDown` and starts a held turn that no finger holds. -/
def p3TraceB : List Event :=
  [.holdBegin false, .endedAddr, .holdEnd, .holdBegin true, .holdEnd, .retryOk false true]

theorem current_violates :
    ((run .cur p1Trace).l.mixed = true ∧ Spec (run .cur p1Trace) = false) ∧
    ((run .cur p3TraceA).h.bad = true ∧ (run .cur p3TraceA).h.finger = false ∧
      (run .cur p3TraceA).l.turn = .phone ∧ Spec (run .cur p3TraceA) = false) ∧
    ((run .cur p3TraceB).h.bad = true ∧ (run .cur p3TraceB).h.finger = false ∧
      (run .cur p3TraceB).h.heldTurn = true ∧ (run .cur p3TraceB).l.turn = .phone ∧
      Spec (run .cur p3TraceB) = false) := by
  decide

/-! ## Proofs over every trace -/

theorem link_run (v : Ver) (hv : v = .old ∨ v = .fix) (evs : List Event) :
    ∀ s : S, s.l.inv = true → (evs.foldl (step v) s).l.inv = true := by
  induction evs with
  | nil => intro s h; exact h
  | cons e rest ih =>
    intro s h
    apply ih
    obtain ⟨⟨t, b, d, dt, mx, sk, rp⟩, r, hh, ff⟩ := s
    rcases hv with rfl | rfl
    · exact link_step_old e _ t b d dt mx sk rp h
    · exact link_step_fix e _ t b d dt mx sk rp h

theorem retry_run (v : Ver) (evs : List Event) :
    ∀ s : S, s.r.inv = true → (evs.foldl (step v) s).r.inv = true := by
  induction evs with
  | nil => intro s h; exact h
  | cons e rest ih =>
    intro s h
    apply ih
    obtain ⟨l, ⟨rt, c⟩, hh, ff⟩ := s
    apply retry_step_inv e _ _ _ _ rt c h
    intro hf
    simp at hf
    exact hf.2

theorem hold_run (evs : List Event) :
    ∀ s : S, s.h.inv = true → (evs.foldl (step .fix) s).h.inv = true := by
  induction evs with
  | nil => intro s h; exact h
  | cons e rest ih =>
    intro s h
    apply ih
    obtain ⟨l, r, ⟨a, b, c, d, i, j, k, m, n⟩, ff⟩ := s
    exact hold_step_fix e _ _ _ _ a b c d i j k m n h

theorem fresh_run (evs : List Event) :
    ∀ s : S, s.f.inv = true → (evs.foldl (step .fix) s).f.inv = true := by
  induction evs with
  | nil => intro s h; exact h
  | cons e rest ih =>
    intro s h
    apply ih
    apply fresh_step _ _ _ _ _ _ _ _ h
    · obtain ⟨⟨t, b, d, dt, mx, sk, rp⟩, r, hh, ff⟩ := s
      exact began_bumps e _ t b d dt mx sk rp
    · intro hr
      cases e with
      | retryOk ends held =>
          simp only [gate, fresh, resumeKind, bne_self_eq_false, Bool.false_or, Bool.and_eq_true] at hr
          exact hr.2.2.1
      | _ => simp at hr

/-- 48df362, for every trace: frames from the stick and the phone never share a press (P1). The review's
suspicion of that version (a phone press during a stick press) does not hold: `phoneMustEndStickTurn`. -/
theorem old_p1_invariant (evs : List Event) : (run .old evs).l.mixed = false := by
  have h := link_run .old (.inl rfl) evs {} (by decide)
  unfold run
  simp only [Link.inv, Bool.and_eq_true] at h
  simpa using h.1.1.1.1.1

/-- The code at 2ad3550, for every trace: at most one address retry starts between two opens (P2). -/
theorem current_p2_invariant (evs : List Event) : (run .cur evs).r.retries ≠ .many := by
  have h := retry_run .cur evs {} (by decide)
  unfold run
  simp only [Retry.inv, Bool.and_eq_true] at h
  simpa using h.1

/-- The fixed code, for every trace: P1, P2, P3 and P4. -/
theorem fixed_invariant (evs : List Event) : Spec (run .fix evs) = true := by
  have h1 := link_run .fix (.inr rfl) evs {} (by decide)
  have h2 := retry_run .fix evs {} (by decide)
  have h3 := hold_run evs {} (by decide)
  have h4 := fresh_run evs {} (by decide)
  unfold run
  simp only [Link.inv, Retry.inv, Hold.inv, Fresh.inv, Bool.and_eq_true] at h1 h2 h3 h4
  simp only [Spec, Bool.and_eq_true]
  exact ⟨⟨⟨h1.1.1.1.1.1, h2.1⟩, h3.1.1.1.1.1⟩, h4.1.1.1⟩

/-- The three counterexamples, on the fixed code. -/
theorem fixed_replays_counterexamples :
    (run .fix p1Trace).l.mixed = false ∧
    (run .fix p3TraceA).h.bad = false ∧ (run .fix p3TraceB).h.bad = false ∧
    (run .fix p3TraceB).l.turn = .none := by
  decide

/-! ## The code at 1dd0f26 breaks P4 -/

/-- P4: an Action Button turn on an open call; eve's address changes, and the retry awaits `refreshAddress()`;
meanwhile the stick talks and finishes; then the refresh returns. The retry starts the lost phone turn after the
owner's newer turn. Replayed in ios/EveLink/Tests/main.swift (`replayP4`). -/
def p4Trace : List Event := [.press, .opened, .endedAddr, .stickTalk, .stickDone, .retryOk true false]

/-- 1dd0f26 keeps P1-P3 on this trace, and breaks P4: the retry restarts its turn (`turn = .phone`). -/
theorem current_retry_clobbers :
    (run .mid p4Trace).f.clobber = true ∧ (run .mid p4Trace).l.turn = .phone ∧
    (run .mid p4Trace).l.mixed = false ∧ (run .mid p4Trace).h.bad = false ∧ Spec (run .mid p4Trace) = false := by
  decide

/-- The fixed code on the P4 trace: the retry starts nothing, and with nothing new in between it still restarts
the lost turn. -/
theorem fixed_replays_p4 :
    (run .fix p4Trace).f.clobber = false ∧ (run .fix p4Trace).l.turn = .none ∧
    (run .fix [.press, .opened, .endedAddr, .retryOk true false]).l.turn = .phone := by
  decide

end Eve.LinkTurn
