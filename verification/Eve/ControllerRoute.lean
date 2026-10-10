/-
  What the controller board is sent, and what it hands the daemon
  (bridge/src/eve_bridge/controller.py: `mirrored`, `Controller.mirror`, `Controller._replay`,
  `Controller._on_message`; and the tee `send_both` in `Daemon.__init__`, bridge/src/eve_bridge/daemon.py).

  The StackChan is the robot. A Home Assistant Voice PE next to it is the "controller": it takes input and
  shows state. `send_both` (daemon.py:137-142) sends every message for the robot to the robot, then passes it
  to `Controller.mirror`. `mirror` keeps the latest message of each mirrored kind in `self.last` and sends it
  at once when the controller is connected. `_replay` sends the kept state; `run` calls it on each connect,
  and the link calls it as `on_boot` when the board prints its boot line. A line from the controller reaches
  the daemon (`on_input`, which is `Daemon._handle_ble`) only when its cmd is in INPUT_CMDS =
  {ptt, key, focus, music}. Since 2026-09-27 the daemon also sends the controller alone the Voice PE's Spotify
  mode (music_mode.py) through `Controller.show`: `music_mode` (kept in `self.last` and replayed), `ring_level`
  and `music_flash` (sent only). `show` is called with nothing else: `MusicMode.send` and the daemon's put-back
  in `_handle_ble` are its only callers.

  The properties, over every trace of mirror(any message), connect, disconnect, boot and a controller
  line(any message):

    (P1) the controller is never sent {"cmd":"sound","on":true}. The owner's rule: beeps only on the StackChan.
    (P2) every line forwarded to the daemon is ptt, key, focus or music. So the controller's acks and status
         never reach the robot's ack waiters.

  The counterexample (commit 4ed166d). MIRRORED_CMDS was {"agent", "sound", "listen"} and `_replay` sent
  only `self.last.values()`. With the controller connected, `mirror({"cmd":"sound","on":true})` sent it to the
  controller (`current_violates`). A second trace: the sound-on is kept while disconnected and replayed on
  connect (`current_violates_on_connect`). The fix (6541fa3, the current file): sound is not mirrored, and
  `_replay` sends SILENT = {"cmd":"sound","on":false} first. P2 held in both versions; `fixed_invariant`
  proves P1 and P2 together for the fixed step.

  The pytest replays against the real class: `test_the_beeps_are_the_robots_alone` (P1: sound-on mirrored
  while connected is not sent, and the first message on connect is SILENT) and
  `test_input_reaches_the_daemon_and_acks_do_not` (P2), both in bridge/tests/test_controller.py.
  `test_controller_show_keeps_the_mode_only` (bridge/tests/test_music_mode.py) replays `display`: only
  `music_mode` is kept, and it is sent again on connect.

  Abstraction and deviations from the Python.
  - A message is its shape only: `sound on`, agent, listen, heartbeat (no "cmd", has "total"), time (no
    "cmd", has "time"), status, ptt, key, focus, ack ({"ack": ...}: no "cmd", no "total", no "time") and
    other (any other cmd). Payloads other than sound's `on` are dropped; they do not bear on P1 or P2.
  - `self.last` is a dict keyed by kind; the model has one slot per kind that `mirrored` admits in either
    version (agent, listen, sound, heartbeat, time). `_replay` sends the slots in a fixed order, not the
    dict's insertion order. P1 does not depend on order.
  - `boot` replays whether or not the model is connected. The link only fires `on_boot` on a boot line read
    from an open port (serial_transport.py:390-394), so this over-approximates the sends.
  - `connect` replays at any time, also when already connected. `run` replays only after
    `wait_connected` returns; this also over-approximates.
  - A send that raises is suppressed in the Python; the model counts every send as reaching the board,
    which is the worst case for P1.
  - `send_audio` (eve's voice for the board's speaker, only {"cmd":"pcm"} and {"cmd":"pcm_flush"} from
    desk_call.BoardSpeaker) is not modelled: it sends no sound command.
  - `mirror` is only reached through `send_both`, so "every message the robot is sent" is the event
    `mirror m` for an arbitrary m.

  Bound: none on the trace. The step lemma is proved for every state and every event, and `fixed_invariant`
  follows by induction over an event list of any length.
-/
namespace Eve.ControllerRoute

inductive Msg where
  | sound (on : Bool)   -- {"cmd":"sound","on":…}
  | agent               -- {"cmd":"agent","state":…}
  | listen              -- {"cmd":"listen","on":…}
  | heartbeat           -- {"total":…,"running":…,…}: no "cmd"
  | time                -- {"time":[…]}: no "cmd"
  | status              -- {"cmd":"status"}
  | ptt                 -- {"cmd":"ptt","on":…}
  | key                 -- {"cmd":"key","name":…}
  | focus               -- {"cmd":"focus",…}
  | music               -- {"cmd":"music",…}: Spotify mode's clicks, holds and dial, from the board
  | musicMode           -- {"cmd":"music_mode","on":…}: to the board, `Controller.show`
  | ringLevel           -- {"cmd":"ring_level",…}: to the board, `Controller.show`
  | musicFlash          -- {"cmd":"music_flash","ok":…}: to the board, `Controller.show`
  | ack                 -- {"ack":…,"ok":…}: no "cmd"
  | other               -- any other cmd ("look", "cam", "explore", …)
  deriving DecidableEq, Repr

inductive Cmd where
  | sound | agent | listen | status | ptt | key | focus | music | musicMode | ringLevel | musicFlash | other
  deriving DecidableEq, Repr

/-- `obj.get("cmd")`. -/
def cmdOf : Msg → Option Cmd
  | .sound _ => some .sound
  | .agent => some .agent
  | .listen => some .listen
  | .status => some .status
  | .ptt => some .ptt
  | .key => some .key
  | .focus => some .focus
  | .music => some .music
  | .musicMode => some .musicMode
  | .ringLevel => some .ringLevel
  | .musicFlash => some .musicFlash
  | .other => some .other
  | .heartbeat | .time | .ack => none

/-- `"total" in obj or "time" in obj`, controller.py `mirrored`. -/
def totalOrTime : Msg → Bool
  | .heartbeat | .time => true
  | _ => false

/-- INPUT_CMDS, controller.py. -/
def inputCmds : List Cmd := [.ptt, .key, .focus, .music]

/-- MIRRORED_CMDS: {agent, sound, listen} at 4ed166d; {agent, listen} now. -/
def mirroredCmds (fixed : Bool) : List Cmd :=
  if fixed then [.agent, .listen] else [.agent, .sound, .listen]

/-- controller.py `mirrored`. -/
def mirrored (fixed : Bool) (m : Msg) : Bool :=
  match cmdOf m with
  | some c => (mirroredCmds fixed).contains c
  | none => totalOrTime m

/-- SILENT, controller.py. -/
def SILENT : Msg := .sound false

structure S where
  conn : Bool := false
  lAgent : Option Msg := none      -- self.last["agent"]
  lListen : Option Msg := none     -- self.last["listen"]
  lSound : Option Msg := none      -- self.last["sound"] (only ever set at 4ed166d)
  lHeart : Option Msg := none      -- self.last["heartbeat"]
  lTime : Option Msg := none       -- self.last["time"]
  lMusic : Option Msg := none      -- self.last["music_mode"] (`show`)
  beeped : Bool := false           -- ghost: the controller was sent {"cmd":"sound","on":true}
  fwdBad : Bool := false           -- ghost: the daemon was handed a line other than ptt, key, focus or music
  deriving DecidableEq, Repr

/-- The owner's rule (P1) and the input rule (P2). -/
def Spec (s : S) : Bool := !s.beeped && !s.fwdBad

/-- The link's `send` to the controller, for a list of messages in order. -/
def sendAll (s : S) (ms : List Msg) : S :=
  { s with beeped := s.beeped || ms.any (· == .sound true) }

/-- `self.last[kind] = obj`, with `kind = cmd or ("time" if "time" in obj else "heartbeat")`
(controller.py `mirror`). Only mirrored messages reach it; any other kind has no slot. -/
def store (s : S) (m : Msg) : S :=
  match m with
  | .agent => { s with lAgent := some m }
  | .listen => { s with lListen := some m }
  | .sound _ => { s with lSound := some m }
  | .heartbeat => { s with lHeart := some m }
  | .time => { s with lTime := some m }
  | _ => s

def slots (s : S) : List (Option Msg) := [s.lAgent, s.lListen, s.lSound, s.lHeart, s.lTime, s.lMusic]

/-- `_replay`: `[SILENT, *self.last.values()]` now; `list(self.last.values())` at 4ed166d. -/
def replay (fixed : Bool) (s : S) : S :=
  sendAll s ((if fixed then [SILENT] else []) ++ (slots s).filterMap id)

/-- `_on_message`: `if obj.get("cmd") in INPUT_CMDS: await self.on_input(obj)`. -/
def forwards (m : Msg) : Bool :=
  match cmdOf m with
  | some c => inputCmds.contains c
  | none => false

/-- P2's set, stated on messages: exactly ptt, key, focus and music. -/
def isInput (m : Msg) : Bool := m == .ptt || m == .key || m == .focus || m == .music

/-- What `Controller.show` is ever called with (music_mode.py `MusicMode.send`, the daemon's put-back). -/
inductive Shown where
  | musicMode | ringLevel | musicFlash
  deriving DecidableEq, Repr

def Shown.msg : Shown → Msg
  | .musicMode => .musicMode
  | .ringLevel => .ringLevel
  | .musicFlash => .musicFlash

/-- `Controller.show`, first half: `music_mode` is kept in `self.last`. -/
def keep (s : S) (k : Shown) : S := if k = .musicMode then { s with lMusic := some k.msg } else s

/-- `Controller.show`, second half: `if self.connected: await self.link.send(obj)`. -/
def sendIf (s : S) (m : Msg) : S := if s.conn then sendAll s [m] else s

def display (s : S) (k : Shown) : S := sendIf (keep s k) k.msg

inductive Event where
  | mirror (m : Msg)   -- `send_both` sent m to the robot, then `Controller.mirror(m)`
  | connect            -- `run`: `wait_connected` returned, then `_replay`
  | disconnect         -- the link closed
  | boot               -- `on_boot` = `_replay`, on the board's boot line
  | line (m : Msg)     -- the controller sent m: `_on_message(m)`
  | display (k : Shown)   -- `Controller.show(k)`: Spotify mode's messages, for the controller alone
  deriving DecidableEq, Repr

def step (fixed : Bool) (s : S) : Event → S
  | .mirror m =>
      if mirrored fixed m then
        let s' := store s m
        if s'.conn then sendAll s' [m] else s'   -- `if self.connected: await self.link.send(obj)`
      else s                                      -- `if not mirrored(obj): return`
  | .connect => replay fixed { s with conn := true }
  | .disconnect => { s with conn := false }
  | .boot => replay fixed s
  | .line m => if forwards m then { s with fwdBad := s.fwdBad || !isInput m } else s
  | .display k => display s k

/-! ## The code at 4ed166d -/

def Cur.run (evs : List Event) : S := evs.foldl (step false) {}

/-- Connected, the owner turns sound on: `send_both` mirrors it and the controller is sent sound-on. -/
theorem current_violates :
    let s := Cur.run [.connect, .mirror (.sound true)]
    s.beeped = true ∧ Spec s = false := by
  decide

/-- The same sound-on, sent while the controller is away, is kept and replayed on connect. -/
theorem current_violates_on_connect :
    let s := Cur.run [.mirror (.sound true), .connect]
    s.beeped = true ∧ Spec s = false := by
  decide

/-- P2 held at 4ed166d too: `_on_message` was the same. -/
theorem current_forwards_only_input : ∀ m : Msg, forwards m = true → isInput m = true := by
  intro m h; cases m <;> simp_all [forwards, cmdOf, inputCmds, isInput]

/-! ## The fixed code (the current file) -/

def Fix.run (evs : List Event) : S := evs.foldl (step true) {}

/-- A slot never holds sound-on. -/
def okSlot (o : Option Msg) : Prop := o ≠ some (.sound true)

def Inv (s : S) : Prop :=
  s.beeped = false ∧ s.fwdBad = false ∧ ∀ o ∈ slots s, okSlot o

theorem filterMap_no_beep : ∀ l : List (Option Msg), (∀ o ∈ l, okSlot o) →
    (l.filterMap id).any (· == .sound true) = false := by
  intro l
  induction l with
  | nil => intro _; rfl
  | cons o rest ih =>
    intro h
    have ho : okSlot o := h o (List.mem_cons_self ..)
    have hr : ∀ o ∈ rest, okSlot o := fun o' hm => h o' (List.mem_cons_of_mem _ hm)
    cases o with
    | none => simpa using ih hr
    | some m =>
      have hm : (m == Msg.sound true) = false := by
        cases m with
        | sound b => cases b <;> simp_all [okSlot]
        | _ => rfl
      simp [List.any_cons, hm, ih hr]

theorem replay_inv (s : S) (h : Inv s) : Inv (replay true s) := by
  obtain ⟨hb, hf, hs⟩ := h
  refine ⟨?_, hf, hs⟩
  simp [replay, sendAll, hb, SILENT, filterMap_no_beep _ hs]

/-- Fixed `mirrored` never admits a sound message. -/
theorem fixed_not_sound (b : Bool) : mirrored true (.sound b) = false := by
  cases b <;> decide

theorem store_inv (s : S) (m : Msg) (hm : m ≠ .sound true) (h : Inv s) : Inv (store s m) := by
  obtain ⟨hb, hf, hs⟩ := h
  have hA := hs s.lAgent (by simp [slots])
  have hL := hs s.lListen (by simp [slots])
  have hS := hs s.lSound (by simp [slots])
  have hH := hs s.lHeart (by simp [slots])
  have hT := hs s.lTime (by simp [slots])
  have hM := hs s.lMusic (by simp [slots])
  have hmo : okSlot (some m) := fun e => hm (Option.some.inj e)
  cases m <;> refine ⟨hb, hf, ?_⟩ <;> intro o ho <;> simp [store, slots] at ho <;>
    rcases ho with ho | ho | ho | ho | ho | ho <;> subst ho <;> assumption

theorem keep_inv (s : S) (k : Shown) (h : Inv s) : Inv (keep s k) := by
  obtain ⟨hb, hf, hs⟩ := h
  unfold keep
  split
  · rename_i hk
    subst hk
    refine ⟨hb, hf, ?_⟩
    intro o ho
    simp only [slots, List.mem_cons, List.mem_nil_iff, or_false] at ho
    rcases ho with ho | ho | ho | ho | ho | ho <;> subst ho
    · exact hs _ (by simp [slots])
    · exact hs _ (by simp [slots])
    · exact hs _ (by simp [slots])
    · exact hs _ (by simp [slots])
    · exact hs _ (by simp [slots])
    · simp [okSlot, Shown.msg]
  · exact ⟨hb, hf, hs⟩

theorem sendIf_inv (s : S) (m : Msg) (hm : (m == Msg.sound true) = false) (h : Inv s) :
    Inv (sendIf s m) := by
  obtain ⟨hb, hf, hs⟩ := h
  unfold sendIf
  split
  · exact ⟨by simp [sendAll, hb, hm], by simp [sendAll, hf], by simpa [sendAll, slots] using hs⟩
  · exact ⟨hb, hf, hs⟩

/-- `Controller.show` sends and keeps only Spotify mode's messages, none of them sound. -/
theorem display_inv (s : S) (k : Shown) (h : Inv s) : Inv (display s k) :=
  sendIf_inv _ _ (by cases k <;> decide) (keep_inv s k h)

theorem inv_step (s : S) (e : Event) (h : Inv s) : Inv (step true s e) := by
  cases e with
  | mirror m =>
    simp only [step]
    split
    · rename_i hmir
      have hm : m ≠ .sound true := by
        intro he; subst he; rw [fixed_not_sound] at hmir; exact Bool.noConfusion hmir
      have h' := store_inv s m hm h
      split
      · obtain ⟨hb, hf, hs⟩ := h'
        refine ⟨?_, hf, hs⟩
        have hmb : (m == Msg.sound true) = false := by
          cases m with
          | sound b => cases b <;> simp_all
          | _ => rfl
        simp [sendAll, hb, hmb]
      · exact h'
    · exact h
  | connect =>
    obtain ⟨hb, hf, hs⟩ := h
    exact replay_inv _ ⟨hb, hf, hs⟩
  | disconnect =>
    obtain ⟨hb, hf, hs⟩ := h
    exact ⟨hb, hf, hs⟩
  | boot => exact replay_inv s h
  | line m =>
    obtain ⟨hb, hf, hs⟩ := h
    simp only [step]
    split
    · rename_i hfw
      refine ⟨hb, ?_, hs⟩
      have : isInput m = true := current_forwards_only_input m hfw
      simp [hf, this]
    · exact ⟨hb, hf, hs⟩
  | display k => exact display_inv s k h

theorem inv_run (evs : List Event) : ∀ s : S, Inv s → Inv (evs.foldl (step true) s) := by
  induction evs with
  | nil => intro s h; exact h
  | cons e rest ih => intro s h; exact ih _ (inv_step s e h)

/-- For every trace of mirrors, connects, disconnects, boots and controller lines: the controller is never
sent sound-on (P1), and the daemon is handed only ptt, key, focus and music (P2). -/
theorem fixed_invariant (evs : List Event) :
    (Fix.run evs).beeped = false ∧ (Fix.run evs).fwdBad = false ∧ Spec (Fix.run evs) = true := by
  have h := inv_run evs {} ⟨rfl, rfl, by intro o ho; simp [slots] at ho; subst ho; simp [okSlot]⟩
  obtain ⟨hb, hf, _⟩ := h
  exact ⟨hb, hf, by simp [Spec, Fix.run, hb, hf]⟩

/-- The counterexample traces on the fixed step: nothing beeps, and the first message on connect is SILENT. -/
theorem fixed_replays_counterexample :
    (Fix.run [.connect, .mirror (.sound true)]).beeped = false ∧
      (Fix.run [.mirror (.sound true), .connect]).beeped = false ∧
      mirrored true (.sound true) = false := by
  decide

end Eve.ControllerRoute
