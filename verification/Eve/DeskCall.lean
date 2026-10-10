/-
  Eve.DeskCall — the board's hold-to-talk button, the Mac microphone, the wake word, and the Telegram chat's
  one call listener, shared by desk calls and phone calls.

  Models:
  - bridge/src/eve_bridge/desk_call.py: `DeskCalls.press` / `_down` / `_up` / `_start`, and the `finally`
    of its `run`;
  - bridge/src/eve_bridge/phone_call.py: `Call.run` (its `listen(say)` and its `finally`) and
    `PhoneCalls._serve`;
  - bridge/src/eve_bridge/telegram.py: `TelegramInlet.listen`, which sets the single `_call_say`;
  - bridge/src/eve_bridge/daemon.py: the `ptt` branch of `_handle_ble`, `_wake_suppressed`, and
    `phone_busy` in `_desk_calls_get`.
  Line numbers are those of the fixed files.

  The properties, over every trace of events:
  - (P1) the desk microphone is open only while the button is held;
  - (P2) the chat's call listener belongs only to a live call, and the most recently started call that is still
    live keeps it: a call that ends never clears a listener that a live call relies on;
  - (P3) the wake word is suppressed while the button is held.
  "The button" is the board's last `ptt` edge (ghost state `button`).

  Findings (all confirmed against the code, each replayed in bridge/tests/test_desk_call_lean.py):
  - P2, the suspected bug. `DeskCalls._down` refuses to start while a phone call is on (`busy`, desk_call.py:268),
    but `PhoneCalls._serve` (phone_call.py:638-646) checks only for another *phone* call. A phone call starting
    during a desk call runs `listen(phone.say)` (phone_call.py:564), replacing the desk's reader. When the desk
    call ended, its `Call.run` `finally` ran `listen(None)`, clearing the PHONE call's reader: the rest of the
    phone call was texted, not spoken. Fix: an ending call passes its own reader, `listen(None, owner=self.say)`
    (phone_call.py:599), and `TelegramInlet.listen` clears only if the caller is still the listener
    (telegram.py:3560).
  - P1, a tap. Every line from the board is its own task (serial_transport.py `asyncio.create_task(on_message)`),
    and `_down` opened the mic only after `await link.set_ring("listening")`, a serial write. A release landing
    during that write ran `_up` first (no mic yet); the press then opened the mic with the button up, and nothing
    closed it until the call ended. Fix: the mic opens before that await (desk_call.py:283-285).
  - P1, a conversation. The `ptt` branch dropped every edge during a conversation. A release during a
    conversation that opened after the press (a think-aloud lesson needs no wake word) left the mic open. Fix:
    only a press is ignored, a release always reaches the desk (daemon.py:2607).
  - P3. `_wake_suppressed` read `DeskCalls.held`, which the call's `finally` clears (desk_call.py:319) and which a
    press during a conversation never sets. A call that ends mid-hold (a stop; live, a press that lands while the
    quiet end's cleanup awaits) brought the wake word back while the button was held. Fix: the daemon keeps the
    board's last edge, `_ptt_held` (daemon.py:2606), and `_wake_suppressed` reads it (daemon.py:994).

  - After 1dd0f26 (CallScope.lean, the hearing call): words heard on a call make that call take the listener back
    (`deskHeard`, `phoneHeard`), so "the last call to take the listener" includes a call that heard words while
    the other held it. P1-P3 still hold for every trace with these events (`fixed_invariant`).

  Abstractions, stated:
  - The chat and the voice are set up (`brain`, `voice` not None), and `listen(say)` returns True.
  - A desk call's start is atomic with the press that starts it: `_start` creates the task, and `Call.run`'s
    `listen(say)` runs on the next loop step. A call's end is atomic: `Call.run`'s `finally` and `run`'s `finally`.
  - `quietEnd` happens only while `held` is false: the keepalive pings while `self.held` (desk_call.py:306), and a
    quiet end needs `QUIET_SECS` (30 s) without a message. `stopEnd` is any other end: `stop`, the hour, a close.
  - A phone call that replaces an older one (phone_call.py:638-644) waits for the old one's `finally`, so it is
    `phoneEnd` followed by `phoneStart`; a `phoneStart` while a phone call is live does nothing.
  - `resume` is the `_down` that was waiting on the ring's serial write going on. In the fixed code nothing the
    properties read happens after that await.
-/
namespace Eve.DeskCall

/-- Who holds the chat's one call listener (`TelegramInlet._call_say`). -/
inductive Holder where
  | nobody | desk | phone
  deriving DecidableEq, Repr

inductive Event where
  | pressOn       -- {"cmd":"ptt","on":true} from the board
  | resume        -- a `_down` waiting on `await link.set_ring("listening")` goes on
  | pressOff      -- {"cmd":"ptt","on":false}
  | quietEnd      -- the desk call ends: QUIET_SECS without a message
  | stopEnd       -- the desk call ends otherwise (stop, the hour, the link closed)
  | phoneStart    -- a phone call takes the chat: its first press (Call._begin_press → _take_chat), since 2026-09-30; before that, PhoneCalls._serve reaching Call.run
  | phoneEnd      -- the phone call's Call.run returns
  | convStart     -- a voice conversation opens (the wake word, or a think-aloud lesson)
  | convEnd
  | deskHeard     -- the desk call's words reach the chat: Call.heard → TelegramInlet.hear(text, call=desk.say)
  | phoneHeard    -- the phone call's words reach the chat: the same, call=phone.say
  deriving DecidableEq, Repr

/-! ## The current code -/

structure Cur where
  button : Bool := false             -- ghost: the board's last ptt edge
  conv : Bool := false               -- daemon._conversation is open
  held : Bool := false               -- DeskCalls.held
  mic : Bool := false                -- a microphone stream is open
  pending : Bool := false            -- a `_down` suspended at the ring's send, its mic not yet opened
  desk : Bool := false               -- a desk call is live (DeskCalls.active)
  phone : Bool := false              -- a phone call is live (PhoneCalls.active)
  listener : Holder := .nobody       -- TelegramInlet._call_say
  last : Holder := .nobody           -- ghost: the call that most recently took the listener
  deriving DecidableEq, Repr

/-- The desk call's end: `Call.run`'s `finally` ran `self.brain.listen(None)`, clearing whoever held it; `run`'s
`finally` closed the mic and set `held = False`. -/
def Cur.endDesk (s : Cur) : Cur :=
  { s with desk := false, mic := false, held := false, listener := .nobody }

def Cur.step (s : Cur) : Event → Cur
  -- daemon `ptt` branch: during a conversation the edge was dropped, press or release.
  -- DeskCalls.press: `if on == self.held: return`, then `self.held = on`.
  -- `_down`: a live call is reused; else `busy()` (a phone call) refuses, else `_start` and `Call.run`'s
  -- `listen(say)`. Then `link.talk()` and `await link.set_ring("listening")`: the mic waits for `resume`.
  | .pressOn =>
      let s := { s with button := true }
      if s.conv || s.held then s
      else
        let s := { s with held := true }
        if s.desk then { s with pending := true }
        else if s.phone then s
        else { s with desk := true, listener := .desk, last := .desk, pending := true }
  -- after the await: `self.mic = self.mic_factory(...)`, `self.mic.open()`, whatever `held` is now.
  | .resume => if s.pending then { s with pending := false, mic := true } else s
  -- `_up`: closes `self.mic`, if there is one yet.
  | .pressOff =>
      let s := { s with button := false }
      if s.conv || !s.held then s else { s with held := false, mic := false }
  | .quietEnd => if s.desk && !s.held then s.endDesk else s
  | .stopEnd => if s.desk then s.endDesk else s
  -- PhoneCalls._serve: no question about a desk call; `Call.run`'s `listen(self.say)` replaces the listener.
  | .phoneStart => if s.phone then s else { s with phone := true, listener := .phone, last := .phone }
  -- the phone's `Call.run` `finally`: `listen(None)`.
  | .phoneEnd => if s.phone then { s with phone := false, listener := .nobody } else s
  | .convStart => { s with conv := true }
  | .convEnd => { s with conv := false }
  -- before the hearing-call fix `hear` never touched the listener.
  | .deskHeard | .phoneHeard => s

def Cur.run (evs : List Event) : Cur := evs.foldl Cur.step {}

/-- `_wake_suppressed`, before the fix: a conversation, or `DeskCalls.held` (the listen key is left out). -/
def Cur.suppressed (s : Cur) : Bool := s.conv || s.held

def Cur.live (s : Cur) : Holder → Bool
  | .nobody => false
  | .desk => s.desk
  | .phone => s.phone

def Cur.p1 (s : Cur) : Bool := !s.mic || s.button
def Cur.p2 (s : Cur) : Bool :=
  (s.listener == .nobody || s.live s.listener) && (!s.live s.last || s.listener == s.last)
def Cur.p3 (s : Cur) : Bool := !s.button || s.suppressed

/-- P2: a desk press starts a desk call, a phone call starts during it, the owner lets go, and the desk call ends
quietly. The phone call is live and was the last to take the chat, but the chat has no listener. -/
def p2Trace : List Event := [.pressOn, .resume, .phoneStart, .pressOff, .quietEnd]

/-- P1: a tap whose release runs while the press waits on the ring's serial write. -/
def p1TapTrace : List Event := [.pressOn, .pressOff, .resume]

/-- P1: a press, a conversation opens, the release is dropped. -/
def p1ConvTrace : List Event := [.pressOn, .resume, .convStart, .pressOff]

/-- P3: a press, then the desk call ends with the button still down. -/
def p3Trace : List Event := [.pressOn, .resume, .stopEnd]

theorem current_violates :
    ((Cur.run p2Trace).phone = true ∧ (Cur.run p2Trace).listener = .nobody ∧ (Cur.run p2Trace).p2 = false) ∧
    ((Cur.run p1TapTrace).mic = true ∧ (Cur.run p1TapTrace).button = false) ∧
    ((Cur.run p1ConvTrace).mic = true ∧ (Cur.run p1ConvTrace).button = false) ∧
    ((Cur.run p3Trace).button = true ∧ (Cur.run p3Trace).suppressed = false) := by
  decide

/-! ## The fixed code -/

structure Fix where
  button : Bool := false             -- the board's last ptt edge: daemon._ptt_held
  conv : Bool := false
  held : Bool := false
  mic : Bool := false
  desk : Bool := false
  phone : Bool := false
  listener : Holder := .nobody
  last : Holder := .nobody           -- ghost
  deriving DecidableEq, Repr

/-- The desk call's end: `listen(None, owner=self.say)` (phone_call.py:599) clears the listener only if it is
still the desk's (telegram.py:3560); `run`'s `finally` closes the mic and sets `held = False` (desk_call.py:319). -/
def Fix.endDesk (s : Fix) : Fix :=
  { s with desk := false, mic := false, held := false,
           listener := if s.listener = .desk then .nobody else s.listener }

def Fix.step (s : Fix) : Event → Fix
  -- daemon.py:2605-2610: `_ptt_held` takes every edge; only a press is ignored during a conversation.
  -- desk_call.py:254-285: as before, but the mic opens before the ring's await.
  | .pressOn =>
      let s := { s with button := true }
      if s.conv || s.held then s
      else
        let s := { s with held := true }
        if s.desk then { s with mic := true }
        else if s.phone then s                                        -- desk_call.py:268, busy
        else { s with desk := true, listener := .desk, last := .desk, mic := true }
  | .resume => s                                                      -- only the ring is left
  -- desk_call.py:287-292 `_up`, reached during a conversation too.
  | .pressOff =>
      let s := { s with button := false }
      if !s.held then s else { s with held := false, mic := false }
  | .quietEnd => if s.desk && !s.held then s.endDesk else s
  | .stopEnd => if s.desk then s.endDesk else s
  | .phoneStart => if s.phone then s else { s with phone := true, listener := .phone, last := .phone }
  -- phone_call.py:599: the phone call clears only its own reader.
  | .phoneEnd =>
      if s.phone then
        { s with phone := false, listener := if s.listener = .phone then .nobody else s.listener }
      else s
  | .convStart => { s with conv := true }
  | .convEnd => { s with conv := false }
  -- telegram.py `hear(…, call=)`: the call that heard the words takes the listener back if it no longer holds it
  -- (CallScope.lean, the hearing call). Only a live call hears: `Call.run`'s `finally` clears its reader and
  -- cancels its `heard` tasks with no await between, so no `hear` of an ended call runs.
  | .deskHeard => if s.desk then { s with listener := .desk, last := .desk } else s
  | .phoneHeard => if s.phone then { s with listener := .phone, last := .phone } else s

def Fix.run (evs : List Event) : Fix := evs.foldl Fix.step {}

/-- `_wake_suppressed` (daemon.py:986-994): a conversation, or `_ptt_held` (the listen key is left out). -/
def Fix.suppressed (s : Fix) : Bool := s.conv || s.button

def Fix.live (s : Fix) : Holder → Bool
  | .nobody => false
  | .desk => s.desk
  | .phone => s.phone

def Fix.p1 (s : Fix) : Bool := !s.mic || s.button
def Fix.p2 (s : Fix) : Bool :=
  (s.listener == .nobody || s.live s.listener) && (!s.live s.last || s.listener == s.last)
def Fix.p3 (s : Fix) : Bool := !s.button || s.suppressed

/-! ### Proof -/

/-- What every reachable fixed state satisfies: the three properties, plus the mic is open only while `held`,
`held` only while the button is down, and the listener is nobody's or the last starter's. -/
def Fix.inv (s : Fix) : Bool :=
  s.p1 && s.p2 && s.p3 && (!s.mic || s.held) && (!s.held || s.button) &&
    (s.listener == .nobody || s.listener == s.last)

theorem inv_init : Fix.inv {} = true := by decide

theorem inv_step (s : Fix) (e : Event) (h : s.inv = true) : (s.step e).inv = true := by
  obtain ⟨button, conv, held, mic, desk, phone, listener, last⟩ := s
  revert h
  cases e <;> cases button <;> cases conv <;> cases held <;> cases mic <;> cases desk <;> cases phone <;>
    cases listener <;> cases last <;> decide

theorem inv_run (evs : List Event) : ∀ s : Fix, s.inv = true → (evs.foldl Fix.step s).inv = true := by
  induction evs with
  | nil => intro s h; exact h
  | cons e rest ih => intro s h; exact ih _ (inv_step s e h)

/-- For every trace of the fixed code: the mic is open only while the button is held (P1); the listener belongs
only to a live call, and the last call to start keeps it while it lives (P2); the wake word is suppressed while
the button is held (P3). -/
theorem fixed_invariant (evs : List Event) :
    (Fix.run evs).p1 = true ∧ (Fix.run evs).p2 = true ∧ (Fix.run evs).p3 = true := by
  have h := inv_run evs {} inv_init
  unfold Fix.run
  simp only [Fix.inv, Bool.and_eq_true] at h
  exact ⟨h.1.1.1.1.1, h.1.1.1.1.2, h.1.1.1.2⟩

/-- The counterexamples, on the fixed code: the phone keeps its listener, the mic is closed with the button up,
and the wake word stays off while the button is down. -/
theorem fixed_replays_counterexamples :
    (Fix.run p2Trace).listener = .phone ∧
    (Fix.run p1TapTrace).mic = false ∧ (Fix.run p1ConvTrace).mic = false ∧
    (Fix.run p3Trace).suppressed = true := by
  decide

end Eve.DeskCall
