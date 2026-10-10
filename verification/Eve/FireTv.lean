/-
  Eve.FireTv — the owner's Fire TV over network ADB: the keys eve sends, the mute toggle and its readback,
  and the one reconnect-and-retry of a shell command.

  Models bridge/src/eve_bridge/firetv.py (line numbers are those of commit 2ad3550):
  - `FireTV.shell` (719-740), with `ensure` (710-717), `_recover` (703-708), `_gone` (600-602), `_lost` (605-607);
  - `FireTV.state` (743-750) and `_send` (784-791);
  - `FireTV._mute` (895-914), the only reader of a toggle;
  - every other command line that goes through `_send` or `shell`: `_power` (840-841, WAKEUP 224 / SLEEP 223),
    `press` (865-866, NAV_KEYS, `n` clamped to 1..10), the media keys (806-807), `_volume` (883),
    `_before_opening` (924, WAKEUP), `_start` (936-937, `am start`), `type_text` (1021, `input text`);
    the reads `_resolve` (947-948) and `packages` (957) send no key and are idempotent.

  The properties, over every trace of commands, adb answers and presses of the TV's own remote:
  - (P1) no command line eve ever sends contains KEYCODE_POWER (26), on any attempt;
  - (P2) a mute or unmute that answers ok *because it read the TV* ("already muted", "TV muted") leaves the TV's
    mute state equal to the target at that moment, for any initial state; a toggle that was lost (the key never
    landed) is reported not-ok by the readback;
  - (P3) one `shell` call with `again=False` delivers its command to the TV at most once, across a timeout and a
    reconnect; it is retried only after a "gone" answer (adb said the TV was not there), and no call makes more
    than two attempts.

  Shape: `code_invariant` (P1, P2 as stated above, P3), proved of the code at 2ad3550 for every trace; and
  `fixed_invariant` (P1, P2 in its strict form below, P3), proved of the fixed code for every trace.

  The fixed code (branch fix/lean-findings, 2026-10-04; its firetv.py lines are 7 below 2ad3550's from `ensure`
  on: `shell` 726-747, `state` 750-757, `_send` 791-798): `_mute` (902-926) keeps the key's `rc`
  (`fail, rc, out = await self._send(…)`, 913); a key the TV refused is not-ok (916-917, "The TV refused the mute
  key"); and a readback that fails or has no `Muted:` is not-ok (918-921, "I pressed mute, but couldn't read the
  TV back, so I can't say it is muted/unmuted."). So ok needs the key taken AND the readback at the target.
  `Fix.step` is that machine (`stepWith shell .fail true`). The rc check goes one step past the fix first proposed
  here (line 909 alone returning not-ok); it only turns an ok into a not-ok, and `fixed_invariant` proves the
  whole of P1-P3 for the code with both. Replays against the real class, bridge/tests/test_firetv.py:
  `test_mute_lost_and_unread_is_not_ok` (`unreadTrace`), `test_mute_refused_key_is_not_ok`
  (`code_violates_refused_key`), and the positive control `test_mute_dropped_key_with_a_readback_says_so`.

  FINDING, in the code at 2ad3550 (proved by `current_violates`; FIXED as above): the strict form of P2 —
  "after mute(target) answers ok, the TV's mute state is the target" — is FALSE. `_mute` lines 907-909: when the
  readback `state()` fails (no answer, a timeout, a reconnect that fails, or an answer without `@@POWER` or
  without `Muted:`), it returns `{"ok": True, "line": "Pressed mute on the TV, but couldn't read it back."}`.
  Line 904 also discards the key's `rc` (`fail, _, _`). So a toggle the TV dropped (rc 0 with no effect, or a
  refused key, rc != 0), followed by a readback that does not come back, is reported ok while the TV's state is
  not the target. The trace is `unreadTrace`: unmuted TV, "mute", read says unmuted, the key is answered but
  lost, the readback times out and the reconnect fails: ok, TV still unmuted. `Fix.step`, the same machine with
  line 909 returning not-ok, proves the strict P2 for every trace (`proposed_fix_invariant`); the code's fix,
  which also checks the rc, proves it with P1 and P3 (`fixed_invariant`).

  P3's history. The task's review notes say the first draft retried after a timeout and so ran a command twice.
  That draft was never committed (firetv.py has one commit, ab16043, already with `again`), so there is no old
  code to put a `current_violates` on. `naive_violates` is the positive control instead: the same `shell` with
  line 729 removed (retry on any loss) delivers a d-pad press twice.

  Abstractions, stated:
  - One command at a time. Concurrent `control` calls (two chats) are outside the model; `ensure` takes the lock
    but the shell command runs outside it.
  - A `shell` call resolves in one step, given the environment's answers for that call (`Run`): whether ensure
    reaches the TV, the first answer, whether the recover reaches it, the second answer. Fields a branch does
    not reach are ignored. The remote may flip the TV's mute between any two shell calls.
  - Answers (`Ans`): `ran` (rc 0, the command took effect), `refused` (rc != 0, not lost: it did not take
    effect), `dropped` (rc 0, but it did not take effect: the lost toggle), `gone` (adb's "device not found" /
    "offline" / "closed" / "protocol fault": `_gone`), `timeoutLanded` and `timeoutLost` (RC_TIMEOUT; the
    command did or did not reach the TV before the runner killed adb). ASSUMPTION: a `gone` answer means the
    command never ran — the docstring of `_gone` says so. If adb can print "error: closed" or "protocol fault"
    after the TV ran the command, a `gone` retry would deliver twice; that is outside the model.
  - `state()` read over a `ran` answer reports the TV's true mute (the `Muted:` line of STREAM_MUSIC); any other
    answered output is a failed read (no `@@POWER`, or no `Muted:`). KEYCODE_VOLUME_MUTE (164) flips that field
    once per delivery; WAKEUP, SLEEP, the d-pad, the media keys, the volume and `am start` do not touch it.
  - `input text` and `am start` inject no keycode by number: their words are shell-quoted (`_sh`, line 179-181),
    and `type_text_arg` (623-633) admits printable ASCII only, so it cannot form a second command.
-/
namespace Eve.FireTv

/-! ## The commands and the keycodes they send -/

inductive NavKey where
  | up | down | left | right | select | back | home | menu
  deriving DecidableEq, Repr

/-- NAV_KEYS, firetv.py:395-396. -/
def navCode : NavKey → Nat
  | .up => 19 | .down => 20 | .left => 21 | .right => 22 | .select => 23 | .back => 4 | .home => 3 | .menu => 82

inductive Media where
  | playPause | play | pause | next | previous | rewind | fastForward | stop
  deriving DecidableEq, Repr

/-- Every `_send` other than mute's, one per command line. -/
inductive Cmd where
  | powerOn                    -- _power(True), 840: input keyevent 224
  | powerOff                   -- _power(False), 840: input keyevent 223
  | press (k : NavKey) (n : Nat) -- press, 865: input keyevent code x n, n clamped to 1..10
  | media (m : Media)          -- 806: cmd media_session dispatch ...
  | volume                     -- 883: cmd media_session volume --set N
  | wake                       -- _before_opening, 924: input keyevent 224
  | amStart                    -- _start, 937: am start ... -n component
  | typeText                   -- type_text, 1021: input text <arg>
  deriving DecidableEq, Repr

def KEYCODE_POWER : Nat := 26

/-- `n = max(1, min(MAX_REPEAT, int(repeat or 1)))`, firetv.py:864. -/
def clampRepeat (n : Nat) : Nat := max 1 (min 10 n)

/-- The keycodes of the command line (`input keyevent ...`); empty for a line that sends none. -/
def keysOf : Cmd → List Nat
  | .powerOn => [224]
  | .powerOff => [223]
  | .press k n => List.replicate (clampRepeat n) (navCode k)
  | .media _ => []
  | .volume => []
  | .wake => [224]
  | .amStart => []
  | .typeText => []

/-- The `again` each line passes to `shell`. -/
def againOf : Cmd → Bool
  | .powerOn => true                                    -- 840-841: default
  | .powerOff => true
  | .press _ _ => false                                 -- 866
  | .media m => m == .play || m == .pause || m == .stop -- 807
  | .volume => true                                     -- 883
  | .wake => true                                       -- 924
  | .amStart => true                                    -- 937
  | .typeText => false                                  -- 1021

def has26 (c : Cmd) : Bool := (keysOf c).contains KEYCODE_POWER

/-- mute's line, 904: `input keyevent 164`. -/
def muteKeys : List Nat := [164]

/-! ## One `shell` call -/

inductive Ans where
  | ran | refused | dropped | gone | timeoutLanded | timeoutLost
  deriving DecidableEq, Repr

/-- `_lost` (605-607): a timeout, or `_gone`. -/
def Ans.lost : Ans → Bool
  | .gone | .timeoutLanded | .timeoutLost => true
  | _ => false

/-- `_gone` (600-602). -/
def Ans.gone? : Ans → Bool
  | .gone => true
  | _ => false

/-- How many times the TV acted on the command for this answer. -/
def Ans.delivers : Ans → Nat
  | .ran | .timeoutLanded => 1
  | _ => 0

/-- What the environment answers during one `shell` call. -/
structure Run where
  reach : Bool := true      -- `ensure` (710-717): connect, else recover, reaches the TV
  first : Ans := .ran       -- 726
  recover : Bool := true    -- `_recover` (733)
  second : Ans := .ran      -- 736
  deriving DecidableEq, Repr

structure Out where
  ok : Bool                 -- the Reach `shell` returns is ok
  ans : Ans                 -- the answer returned with it (meaningful when ok)
  delivered : Nat           -- how many times the TV acted on the command
  attempts : Nat            -- how many `adb shell` the call ran
  retriedNotGone : Bool     -- a second attempt followed a first answer that was not `gone`
  ready : Bool              -- `_ready` afterwards
  deriving DecidableEq, Repr

/-- `FireTV.shell`, 719-740, line by line. -/
def shell (ready again : Bool) (r : Run) : Out :=
  -- 723-725: ensure; `_ready` short-cuts it (712-713)
  if !(ready || r.reach) then ⟨false, .ran, 0, 0, false, false⟩
  else
    let a := r.first                                                    -- 726
    if !a.lost then ⟨true, a, a.delivers, 1, false, true⟩               -- 727-728
    else if !again && !a.gone? then ⟨false, a, a.delivers, 1, false, false⟩   -- 729-731
    else if !r.recover then ⟨false, a, a.delivers, 1, false, false⟩      -- 732-735
    else
      let b := r.second                                                 -- 736
      let notGone := !again && !a.gone?
      if b.lost then ⟨false, b, a.delivers + b.delivers, 2, notGone, false⟩   -- 737-739
      else ⟨true, b, a.delivers + b.delivers, 2, notGone, true⟩          -- 740

/-- The draft the review describes: line 729 missing, so a timeout is retried whatever `again` says. -/
def naiveShell (ready again : Bool) (r : Run) : Out :=
  if !(ready || r.reach) then ⟨false, .ran, 0, 0, false, false⟩
  else
    let a := r.first
    if !a.lost then ⟨true, a, a.delivers, 1, false, true⟩
    else if !r.recover then ⟨false, a, a.delivers, 1, false, false⟩
    else
      let b := r.second
      let notGone := !again && !a.gone?
      if b.lost then ⟨false, b, a.delivers + b.delivers, 2, notGone, false⟩
      else ⟨true, b, a.delivers + b.delivers, 2, notGone, true⟩

/-! ## The command machine -/

/-- What the current call is waiting on: the next `shell` it runs. -/
inductive Job where
  | idle
  | readMute (t : Bool)                  -- _mute's first state(), 896
  | toggle (t : Bool)                    -- _mute's keyevent 164, 904
  | readBack (t : Bool)                  -- _mute's after state(), 907
  | send (again h26 : Bool)              -- any other _send: its `again`, and whether its line has keycode 26
  deriving DecidableEq, Repr

inductive Said where
  | nothing
  | ok          -- ok, resting on what the TV said (a read, or an answered command)
  | okUnread    -- 909: "Pressed mute on the TV, but couldn't read it back."
  | fail
  deriving DecidableEq, Repr

structure S where
  muted : Bool := false        -- the TV
  ready : Bool := false        -- FireTV._ready
  job : Job := .idle
  said : Said := .nothing      -- the last answer to the owner
  -- ghosts
  sent26 : Bool := false       -- a line with KEYCODE_POWER went out
  dup : Bool := false          -- an again=False call reached the TV twice
  tooMany : Bool := false      -- a call made a third attempt
  retriedNotGone : Bool := false -- an again=False call was retried after an answer that was not `gone`
  falseRead : Bool := false    -- a mute answered ok from a read while the TV was not at its target
  falseOk : Bool := false      -- a mute answered ok (any ok) while the TV was not at its target
  deriving DecidableEq, Repr

inductive Event where
  | cmd (c : Cmd)              -- a control call other than mute/unmute
  | mute (t : Bool)            -- control("mute") (t = true) / control("unmute") (t = false), 817-818
  | run (r : Run)              -- the environment's answers to the current call's next shell
  | remote                     -- the TV's own remote presses mute
  deriving DecidableEq, Repr

/-- Toggle `n` times. -/
def flips : Nat → Bool → Bool
  | 0, b => b
  | n + 1, b => flips n (!b)

/-- The ghosts every shell call updates. -/
def S.audit (s : S) (again h26 : Bool) (o : Out) : S :=
  { s with sent26 := s.sent26 || (h26 && decide (0 < o.attempts)),
           dup := s.dup || (!again && decide (2 ≤ o.delivered)),
           tooMany := s.tooMany || decide (2 < o.attempts),
           retriedNotGone := s.retriedNotGone || o.retriedNotGone }

/-- The end of a mute: what is said, and the ghosts that read it against the TV. -/
def S.answer (s : S) (t : Bool) (w : Said) : S :=
  { s with job := .idle, said := w,
           falseRead := s.falseRead || (w == .ok && s.muted != t),
           falseOk := s.falseOk || ((w == .ok || w == .okUnread) && s.muted != t) }

/-- The step, parametrised by `shell`, by what an unread readback says (2ad3550 line 909; fixed 919-921), and by
whether the key's rc is checked (fixed 913, 916-917; 2ad3550 line 904 discarded it). -/
def stepWith (sh : Bool → Bool → Run → Out) (unread : Said) (rcChecked : Bool) (s : S) : Event → S
  | .cmd c => if s.job == .idle then { s with job := .send (againOf c) (has26 c), said := .nothing } else s
  | .mute t => if s.job == .idle then { s with job := .readMute t, said := .nothing } else s
  | .remote => { s with muted := !s.muted }
  | .run r =>
    match s.job with
    | .idle => s
    | .send again h =>
        let o := sh s.ready again r
        let s := (s.audit again h o)
        { s with ready := o.ready, job := .idle, said := if o.ok then .ok else .fail }
    | .readMute t =>
        -- 896-902: state() is shell(STATE_SCRIPT) (again=True), then the @@POWER and Muted: checks
        let o := sh s.ready true r
        let s := { s.audit true false o with ready := o.ready }
        if !o.ok then s.answer t .fail                                -- 897-898
        else if o.ans != .ran then { s with ready := false }.answer t .fail   -- 747-749, 899-900
        else if s.muted == t then s.answer t .ok                      -- 901-902: already
        else { s with job := .toggle t }
    | .toggle t =>
        -- 2ad3550 904-906 (fixed 913-915): input keyevent 164, again=False
        let o := sh s.ready false r
        let s := { s.audit false false o with ready := o.ready, muted := flips o.delivered s.muted }
        if !o.ok then s.answer t .fail
        -- fixed 916-917: `if rc != 0`: the TV refused the key (2ad3550 discarded the rc)
        else if rcChecked && o.ans == .refused then s.answer t .fail
        else { s with job := .readBack t }
    | .readBack t =>
        -- 907-914
        let o := sh s.ready true r
        let s := { s.audit true false o with ready := o.ready }
        if !o.ok then s.answer t unread                               -- 745-746, 908-909
        else if o.ans != .ran then { s with ready := false }.answer t unread  -- 747-749, 908-909
        else if s.muted != t then s.answer t .fail                    -- 910-912
        else s.answer t .ok                                           -- 913-914

/-- The code at 2ad3550: line 909 answers ok, and line 904 discards the rc. -/
def step : S → Event → S := stepWith shell .okUnread false
def run (evs : List Event) : S := evs.foldl step {}

/-- The fix first proposed here: line 909 answers not-ok, the rc still discarded. -/
def Proposed.step : S → Event → S := stepWith shell .fail false
def Proposed.run (evs : List Event) : S := evs.foldl Proposed.step {}

/-- The fixed code: an unread readback is not-ok (919-921), and so is a refused key (916-917). -/
def Fix.step : S → Event → S := stepWith shell .fail true
def Fix.run (evs : List Event) : S := evs.foldl Fix.step {}

/-- The review's draft: line 729 missing. -/
def Naive.step : S → Event → S := stepWith naiveShell .okUnread false
def Naive.run (evs : List Event) : S := evs.foldl Naive.step {}

/-- P1: no line with keycode 26; P2: no ok from a read while the TV is off its target; P3: no again=False call
reached the TV twice, none was retried after a non-gone answer, none made a third attempt. -/
def Spec (s : S) : Bool :=
  !s.sent26 && !s.falseRead && !s.dup && !s.retriedNotGone && !s.tooMany

/-! ## Lemmas -/

theorem replicate_no26 (m x : Nat) (hx : x ≠ 26) : (List.replicate m x).contains 26 = false := by
  induction m with
  | zero => rfl
  | succ m ih =>
    simp only [List.replicate_succ, List.contains_cons, ih, Bool.or_false]
    simp only [beq_eq_false_iff_ne, ne_eq]
    exact fun h => hx h.symm

/-- P1 at the source: no command line eve builds contains keycode 26. -/
theorem has26_never (c : Cmd) : has26 c = false := by
  cases c with
  | press k n => cases k <;> exact replicate_no26 _ _ (by decide)
  | _ => rfl

theorem muteKeys_no26 : muteKeys.contains KEYCODE_POWER = false := by decide

/-- P3 for one call, every environment: with again=False, at most one delivery and no retry after a non-gone
answer; never more than two attempts. -/
theorem shell_sound (ready again : Bool) (r : Run) :
    (shell ready again r).attempts ≤ 2 ∧
    (again = false → (shell ready again r).delivered ≤ 1) ∧
    (shell ready again r).retriedNotGone = false := by
  obtain ⟨a, b, c, d⟩ := r
  cases ready <;> cases again <;> cases a <;> cases b <;> cases c <;> cases d <;> decide

/-! ## The invariant -/

/-- Spec, and no pending send carries keycode 26. -/
def Inv (s : S) : Prop :=
  s.sent26 = false ∧ s.falseRead = false ∧ s.dup = false ∧ s.retriedNotGone = false ∧ s.tooMany = false ∧
    ∀ a, s.job ≠ .send a true

/-- Inv is kept by the step for any unread answer other than `.ok` and either rc rule: the code at 2ad3550, the
first proposal and the fixed code alike. -/
theorem inv_step_with (unread : Said) (hu : unread ≠ .ok) (rc : Bool) (s : S) (e : Event) (h : Inv s) :
    Inv (stepWith shell unread rc s e) := by
  obtain ⟨muted, ready, job, said, g1, g2, g3, g4, g5, g6⟩ := s
  obtain ⟨h1, h2, h3, h4, h5, h6⟩ := h
  simp only at h1 h2 h3 h4 h5 h6
  subst h1 h2 h3 h4 h5
  cases e with
  | cmd c =>
    simp only [stepWith]
    split
    · refine ⟨rfl, rfl, rfl, rfl, rfl, ?_⟩
      intro a hj; simp at hj; rw [has26_never] at hj; exact Bool.false_ne_true hj.2
    · exact ⟨rfl, rfl, rfl, rfl, rfl, h6⟩
  | mute t =>
    simp only [stepWith]
    split
    · exact ⟨rfl, rfl, rfl, rfl, rfl, by intro a; simp⟩
    · exact ⟨rfl, rfl, rfl, rfl, rfl, h6⟩
  | remote => exact ⟨rfl, rfl, rfl, rfl, rfl, h6⟩
  | run r =>
    cases job with
    | idle => exact ⟨rfl, rfl, rfl, rfl, rfl, h6⟩
    | readMute t =>
      obtain ⟨hA, -, hR⟩ := shell_sound ready true r
      simp only [stepWith]
      generalize shell ready true r = o at *
      repeat' split
      all_goals
        refine ⟨?_, ?_, ?_, ?_, ?_, ?_⟩ <;> simp_all [S.answer, S.audit] <;> (try cases unread) <;> simp_all <;> omega
    | toggle t =>
      obtain ⟨hA, hD, hR⟩ := shell_sound ready false r
      have hD := hD rfl
      simp only [stepWith]
      generalize shell ready false r = o at *
      repeat' split
      all_goals
        refine ⟨?_, ?_, ?_, ?_, ?_, ?_⟩ <;> simp_all [S.answer, S.audit] <;> (try cases unread) <;> simp_all <;> omega
    | readBack t =>
      obtain ⟨hA, -, hR⟩ := shell_sound ready true r
      simp only [stepWith]
      generalize shell ready true r = o at *
      repeat' split
      all_goals
        refine ⟨?_, ?_, ?_, ?_, ?_, ?_⟩ <;> simp_all [S.answer, S.audit] <;> (try cases unread) <;> simp_all <;> omega
    | send again hh =>
      have hh' : hh = false := by
        cases hh
        · rfl
        · exact absurd rfl (h6 again)
      subst hh'
      obtain ⟨hA, hD, hR⟩ := shell_sound ready again r
      simp only [stepWith]
      generalize shell ready again r = o at *
      cases again
      · have hD := hD rfl
        refine ⟨?_, ?_, ?_, ?_, ?_, ?_⟩ <;> simp_all [S.audit] <;> omega
      · refine ⟨?_, ?_, ?_, ?_, ?_, ?_⟩ <;> simp_all [S.audit] <;> omega

theorem inv_step (s : S) (e : Event) (h : Inv s) : Inv (step s e) :=
  inv_step_with .okUnread (by decide) false s e h

theorem inv_run (evs : List Event) : ∀ s : S, Inv s → Inv (evs.foldl step s) := by
  induction evs with
  | nil => intro s h; exact h
  | cons e rest ih => intro s h; exact ih _ (inv_step s e h)

/-- For every trace of commands, adb answers, reconnects and remote presses, on the code as it is:
(P1) no line with KEYCODE_POWER is ever sent; (P2) no mute or unmute answers ok from a read of the TV while the
TV is not at its target, so a lost toggle is reported not-ok by a readback that comes back; (P3) no again=False
call reaches the TV twice, none is retried after an answer other than "gone", and none makes a third attempt. -/
theorem code_invariant (evs : List Event) : Spec (run evs) = true := by
  have h := inv_run evs {} ⟨rfl, rfl, rfl, rfl, rfl, by intro a; simp⟩
  obtain ⟨h1, h2, h3, h4, h5, -⟩ := h
  simp [Spec, run, h1, h2, h3, h4, h5]

theorem fix_inv_run (evs : List Event) : ∀ s : S, Inv s → Inv (evs.foldl Fix.step s) := by
  induction evs with
  | nil => intro s h; exact h
  | cons e rest ih => intro s h; exact ih _ (inv_step_with .fail (by decide) true s e h)

/-- P3, the "gone" half, for every call: a gone answer is retried, and a call never makes a third attempt. -/
theorem gone_retried_once (ready again : Bool) (r : Run) (hr : ready || r.reach) (hg : r.first = .gone)
    (hrec : r.recover = true) : (shell ready again r).attempts = 2 := by
  obtain ⟨a, b, c, d⟩ := r
  simp only at hg hrec
  subst hg hrec
  cases ready <;> cases a <;> cases again <;> cases d <;> simp_all [shell, Ans.lost, Ans.gone?]

/-! ## The bug in the current code: an unread readback answers ok -/

/-- Unmuted TV; "mute"; the read says unmuted; the key is answered with rc 0 but the TV drops it; the readback
times out and the reconnect fails. -/
def unreadTrace : List Event :=
  [.mute true, .run {}, .run { first := .dropped }, .run { first := .timeoutLost, recover := false }]

theorem code_violates_unread_ok :
    (run unreadTrace).said = .okUnread ∧ (run unreadTrace).muted = false ∧ (run unreadTrace).falseOk = true := by
  decide

/-- The same with a refused key (rc != 0, discarded by line 904) and a readback without `Muted:`. -/
theorem code_violates_refused_key :
    let s := run [.mute true, .run {}, .run { first := .refused }, .run { first := .refused }]
    s.said = .okUnread ∧ s.muted = false ∧ s.falseOk = true := by
  decide

/-- With an unread readback answering not-ok, under either rc rule, no step makes a false ok. -/
theorem falseOk_step (rc : Bool) (s : S) (e : Event) (h : s.falseOk = false) :
    (stepWith shell .fail rc s e).falseOk = false := by
    cases e with
    | cmd c => simp only [stepWith]; split <;> simp_all
    | mute t => simp only [stepWith]; split <;> simp_all
    | remote => simp_all [stepWith]
    | run r =>
      simp only [stepWith]
      rcases hj : s.job with _ | t | t | t | ⟨again, hh⟩
      · simp_all
      · simp only; split
        · simp_all [S.answer, S.audit]
        · split
          · simp_all [S.answer, S.audit]
          · split
            · rename_i hm; cases t <;> cases hmu : s.muted <;> simp_all [S.answer, S.audit]
            · simp_all [S.audit]
      · simp only; split
        · simp_all [S.answer, S.audit]
        · split
          · simp_all [S.answer, S.audit]
          · simp_all [S.audit]
      · simp only; split
        · simp_all [S.answer, S.audit]
        · split
          · simp_all [S.answer, S.audit]
          · split
            · simp_all [S.answer, S.audit]
            · rename_i hm; cases t <;> cases hmu : (flips 0 s.muted) <;> simp_all [S.answer, S.audit]
      · simp_all [S.audit]

/-- The first proposal (line 909 answers not-ok): every ok from mute/unmute leaves the TV at its target, for
every trace. -/
theorem proposed_fix_invariant (evs : List Event) : (Proposed.run evs).falseOk = false := by
  suffices ∀ s : S, s.falseOk = false → (evs.foldl Proposed.step s).falseOk = false from this {} rfl
  induction evs with
  | nil => intro s h; exact h
  | cons e rest ih => intro s h; exact ih _ (falseOk_step false s e h)

/-- The fixed code, for every trace of commands, adb answers, reconnects and remote presses: (P1) no line with
KEYCODE_POWER; (P2, strict) every ok from mute or unmute, read or not, leaves the TV at its target, so a lost or
refused toggle is never reported ok; (P3) no again=False call reaches the TV twice, none is retried after an
answer other than "gone", none makes a third attempt. -/
theorem fix_falseOk_run (evs : List Event) : ∀ s : S, s.falseOk = false → (evs.foldl Fix.step s).falseOk = false := by
  induction evs with
  | nil => intro s h; exact h
  | cons e rest ih => intro s h; exact ih _ (falseOk_step true s e h)

theorem fixed_invariant (evs : List Event) : Spec (Fix.run evs) = true ∧ (Fix.run evs).falseOk = false := by
  have h := fix_inv_run evs {} ⟨rfl, rfl, rfl, rfl, rfl, by intro a; simp⟩
  obtain ⟨h1, h2, h3, h4, h5, -⟩ := h
  exact ⟨by simp [Spec, Fix.run, h1, h2, h3, h4, h5], fix_falseOk_run evs {} rfl⟩

/-- Both traces that break the code at 2ad3550: an ok while the TV is off its target. -/
theorem current_violates :
    (run unreadTrace).falseOk = true ∧
    (run [.mute true, .run {}, .run { first := .refused }, .run { first := .refused }]).falseOk = true := by
  decide

theorem fix_replays_unread : (Fix.run unreadTrace).said = .fail ∧ (Fix.run unreadTrace).falseOk = false := by
  decide

/-- The refused key on the fixed code: not-ok at the key (916-917), before any readback; the first proposal only
got there through the unread readback. -/
theorem fix_replays_refused :
    let evs : List Event := [.mute true, .run {}, .run { first := .refused }]
    (Fix.run evs).said = .fail ∧ (Fix.run evs).job = .idle ∧ (Proposed.run evs).job = .readBack true := by
  decide

/-! ## Worked traces -/

/-- A mute whose key times out is not-ok and is not sent again; the TV may be muted, but nobody was told so. -/
theorem toggle_timeout_not_sent_again :
    let s := run [.mute true, .run {}, .run { first := .timeoutLanded }]
    s.said = .fail ∧ s.muted = true ∧ s.dup = false := by
  decide

/-- A mute whose first try finds the TV gone is sent once more after the reconnect, and confirmed. -/
theorem toggle_gone_retried_once :
    let s := run [.mute true, .run {}, .run { first := .gone }, .run {}]
    s.said = .ok ∧ s.muted = true := by
  decide

/-- The remote mutes the TV between eve's read and eve's key: the key flips it back to unmuted, and the
readback reports not-ok rather than a false "TV muted". -/
theorem remote_race_reported :
    let s := run [.mute true, .run {}, .remote, .run {}, .run {}]
    s.said = .fail ∧ s.muted = false ∧ s.falseRead = false := by
  decide

/-- Positive control for P3: without line 729, a d-pad press that timed out after landing is pressed again. -/
theorem naive_violates :
    let s := Naive.run [.cmd (.press .down 1), .run { first := .timeoutLanded }]
    s.dup = true := by
  decide

/-- The same trace on the code: one press, and "I didn't send it again". -/
theorem code_replays_naive :
    let s := run [.cmd (.press .down 1), .run { first := .timeoutLanded }]
    s.dup = false ∧ s.said = .fail := by
  decide

end Eve.FireTv
