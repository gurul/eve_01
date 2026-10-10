/-
Eve.Serial — the host side of the serial link to the robot.

Three models, each of the code as it was (`…Cur`) and as fixed (`…Fix`):

  A. EveSerial.send's write lock (bridge/src/eve_bridge/serial_transport.py).
     Invariant: at most one `ser.write` runs on the port at any time.
  B. The status-ack watchdog (bridge/src/eve_bridge/daemon.py, _status_poller
     and the status-ack branch of _handle_ble).
     Invariant: a link that flaps (never two consecutive answered watchdog polls
     between escalations) reaches the RTS pulse on every second escalation.
  C. Request/ack matching for the folder push (folder_push._send_expect,
     Daemon.wait_for_ack / expect_ack, the ack router in _handle_ble).
     Invariant: an ack is never dropped while its request is outstanding, and a
     waiter only ever receives the ack for its own request.

Core Lean only. Every theorem is closed by `decide`, `rfl`, `simp`, `omega` or
induction; nothing uses `native_decide`.
-/

namespace Eve.Serial

/-- Run a step function over an event list. -/
def run {σ ε : Type} (step : σ → ε → σ) (s : σ) (es : List ε) : σ := es.foldl step s

theorem run_inv {σ ε : Type} (P : σ → Prop) (step : σ → ε → σ)
    (h : ∀ s e, P s → P (step s e)) : ∀ (es : List ε) (s : σ), P s → P (run step s es) := by
  intro es
  induction es with
  | nil => intro s hs; exact hs
  | cons e es ih => intro s hs; exact ih (step s e) (h s e hs)

theorem run_append {σ ε : Type} (step : σ → ε → σ) (s : σ) (a b : List ε) :
    run step s (a ++ b) = run step (run step s a) b := by
  simp [run, List.foldl_append]

/-! ## A. Writes on the port

Senders are numbered. `lock` is `_send_lock`'s holder; `writes` lists the
senders whose `ser.write` is running on an executor thread right now.

  acq i     serial_transport.py:195   `async with self._send_lock:` acquires
  submit i  serial_transport.py:202   `run_in_executor(None, ser.write, data)`
  finish i  the executor thread's `ser.write` returns
  cancel i  the sending task is cancelled (daemon.py `_send_cam`:
            `asyncio.wait_for(self.ble.send(...), timeout=2.0)`)

Current code: a cancel while awaiting the executor raises CancelledError out of
the `await`, and `async with` releases the lock at once, while the thread is
still inside `ser.write`. -/

inductive Phase
  | idle      -- not sending
  | holding   -- holds the lock, write not yet submitted
  | awaiting  -- holds the lock, awaiting its executor write
  | draining  -- (fix only) cancelled, still holds the lock until the write returns
  deriving DecidableEq, Repr

structure WState where
  lock : Option Nat
  phase : Nat → Phase
  writes : List Nat

inductive WEv
  | acq (i : Nat)
  | submit (i : Nat)
  | finish (i : Nat)
  | cancel (i : Nat)

def upd (f : Nat → Phase) (i : Nat) (p : Phase) : Nat → Phase :=
  fun k => if k = i then p else f k

def wInit : WState := ⟨none, fun _ => .idle, []⟩

/-- Shared: acquire and submit are unchanged by the fix. -/
def wAcq (s : WState) (i : Nat) : WState :=
  if s.lock = none ∧ s.phase i = .idle then
    { s with lock := some i, phase := upd s.phase i .holding } else s

def wSubmit (s : WState) (i : Nat) : WState :=
  if s.phase i = .holding then
    { s with phase := upd s.phase i .awaiting, writes := i :: s.writes } else s

/-- Current code (serial_transport.py:195-203). -/
def wStepCur (s : WState) : WEv → WState
  | .acq i => wAcq s i
  | .submit i => wSubmit s i
  | .finish i =>
    -- the thread returns; if its sender is still awaiting, `async with` exits
    if i ∈ s.writes then
      if s.phase i = .awaiting then
        { s with writes := s.writes.erase i, lock := none, phase := upd s.phase i .idle }
      else { s with writes := s.writes.erase i }
    else s
  | .cancel i =>
    -- CancelledError leaves the `async with` block: lock released, thread keeps writing
    if s.phase i = .holding ∨ s.phase i = .awaiting then
      { s with lock := none, phase := upd s.phase i .idle }
    else s

/-- Fixed code: `_write_holding_lock` waits for the thread's write before the
lock is released, however many times the sender is cancelled. -/
def wStepFix (s : WState) : WEv → WState
  | .acq i => wAcq s i
  | .submit i => wSubmit s i
  | .finish i =>
    if i ∈ s.writes then
      if s.phase i = .awaiting ∨ s.phase i = .draining then
        { s with writes := s.writes.erase i, lock := none, phase := upd s.phase i .idle }
      else { s with writes := s.writes.erase i }
    else s
  | .cancel i =>
    if s.phase i = .holding then
      { s with lock := none, phase := upd s.phase i .idle }   -- no write submitted yet
    else if s.phase i = .awaiting then
      { s with phase := upd s.phase i .draining }             -- keep the lock
    else s

/-- The counterexample trace: sender 0 starts a write and is cancelled by
`wait_for`; sender 1 takes the freed lock and starts its own write while
sender 0's thread is still inside `ser.write`. -/
def aTrace : List WEv := [.acq 0, .submit 0, .cancel 0, .acq 1, .submit 1]

theorem A_current_two_writes : (run wStepCur wInit aTrace).writes.length = 2 := by decide

def WInv (s : WState) : Prop :=
  (∀ j, s.phase j ≠ .idle → s.lock = some j) ∧
  (s.writes = [] ∨ ∃ j, s.writes = [j]) ∧
  (∀ j, (s.phase j = .awaiting ∨ s.phase j = .draining) ↔ s.writes = [j])

theorem wInit_inv : WInv wInit := by
  refine ⟨?_, Or.inl rfl, ?_⟩
  · intro j hj; exact absurd rfl hj
  · intro j; simp [wInit]

theorem wAcq_inv (s : WState) (i : Nat) (h : WInv s) : WInv (wAcq s i) := by
  obtain ⟨h1, h2, h3⟩ := h
  unfold wAcq
  split
  · rename_i hc
    obtain ⟨hl, hp⟩ := hc
    refine ⟨?_, h2, ?_⟩
    · intro j hj
      by_cases hji : j = i
      · subst hji; rfl
      · simp only [upd, hji, ite_false] at hj
        have := h1 j hj
        rw [hl] at this; cases this
    · intro j
      by_cases hji : j = i
      · subst hji
        simp only [upd, ite_true]
        constructor
        · intro hh; cases hh <;> contradiction
        · intro hw
          have := (h3 j).mpr hw
          rw [hp] at this
          cases this <;> contradiction
      · simp only [upd, hji, ite_false]; exact h3 j
  · exact ⟨h1, h2, h3⟩

theorem wSubmit_inv (s : WState) (i : Nat) (h : WInv s) : WInv (wSubmit s i) := by
  obtain ⟨h1, h2, h3⟩ := h
  unfold wSubmit
  split
  · rename_i hp
    have hli : s.lock = some i := h1 i (by rw [hp]; decide)
    have hw : s.writes = [] := by
      rcases h2 with h2 | ⟨k, hk⟩
      · exact h2
      · have hk' := (h3 k).mpr hk
        have hlk : s.lock = some k := h1 k (by rcases hk' with h | h <;> rw [h] <;> decide)
        rw [hli] at hlk
        cases hlk
        rw [hp] at hk'
        rcases hk' with h | h <;> cases h
    refine ⟨?_, ?_, ?_⟩
    · intro j hj
      by_cases hji : j = i
      · subst hji; exact hli
      · simp only [upd, hji, ite_false] at hj; exact h1 j hj
    · exact Or.inr ⟨i, by rw [hw]⟩
    · intro j
      by_cases hji : j = i
      · subst hji; simp [upd, hw]
      · simp only [upd, hji, ite_false, hw]
        constructor
        · intro hh; have := (h3 j).mp hh; rw [hw] at this; cases this
        · intro hh; cases hh; exact absurd rfl hji
  · exact ⟨h1, h2, h3⟩

theorem wStepFix_inv (s : WState) (e : WEv) (h : WInv s) : WInv (wStepFix s e) := by
  cases e with
  | acq i => exact wAcq_inv s i h
  | submit i => exact wSubmit_inv s i h
  | finish i =>
    obtain ⟨h1, h2, h3⟩ := h
    simp only [wStepFix]
    split
    · rename_i hin
      have hwi : s.writes = [i] := by
        rcases h2 with h2 | ⟨k, hk⟩
        · rw [h2] at hin; cases hin
        · rw [hk] at hin
          have : i = k := by simpa using hin
          subst this; exact hk
      have hpi := (h3 i).mpr hwi
      split
      · have hli : s.lock = some i := h1 i (by rcases hpi with h | h <;> rw [h] <;> decide)
        refine ⟨?_, Or.inl (by simp [hwi]), ?_⟩
        · intro j hj
          by_cases hji : j = i
          · subst hji; simp [upd] at hj
          · simp only [upd, hji, ite_false] at hj
            have := h1 j hj
            rw [hli] at this
            cases this; exact absurd rfl hji
        · intro j
          by_cases hji : j = i
          · subst hji; simp [upd, hwi]
          · simp only [upd, hji, ite_false, hwi, List.erase_cons_head]
            constructor
            · intro hh
              have := (h3 j).mp hh
              rw [hwi] at this; cases this; exact absurd rfl hji
            · intro hh; cases hh
      · contradiction
    · exact ⟨h1, h2, h3⟩
  | cancel i =>
    obtain ⟨h1, h2, h3⟩ := h
    simp only [wStepFix]
    split
    · rename_i hp
      have hli : s.lock = some i := h1 i (by rw [hp]; decide)
      refine ⟨?_, h2, ?_⟩
      · intro j hj
        by_cases hji : j = i
        · subst hji; simp [upd] at hj
        · simp only [upd, hji, ite_false] at hj
          have := h1 j hj
          rw [hli] at this; cases this; exact absurd rfl hji
      · intro j
        by_cases hji : j = i
        · subst hji
          simp only [upd, ite_true]
          constructor
          · intro hh; rcases hh with hh | hh <;> cases hh
          · intro hw
            have := (h3 j).mpr hw
            rw [hp] at this
            rcases this with h | h <;> cases h
        · simp only [upd, hji, ite_false]; exact h3 j
    · split
      · rename_i _ hp
        refine ⟨?_, h2, ?_⟩
        · intro j hj
          by_cases hji : j = i
          · subst hji; exact h1 j (by rw [hp]; decide)
          · simp only [upd, hji, ite_false] at hj; exact h1 j hj
        · intro j
          by_cases hji : j = i
          · subst hji
            simp only [upd, ite_true]
            constructor
            · intro _; exact (h3 j).mp (Or.inl hp)
            · intro _; simp
          · simp only [upd, hji, ite_false]; exact h3 j
      · exact ⟨h1, h2, h3⟩

/-- A, fixed: for EVERY interleaving of acquires, submits, thread returns and
cancellations, by any number of senders, at most one write is on the port. -/
theorem A_fixed_single_writer (es : List WEv) :
    (run wStepFix wInit es).writes.length ≤ 1 := by
  have h := run_inv WInv wStepFix wStepFix_inv es wInit wInit_inv
  rcases h.2.1 with h0 | ⟨j, hj⟩
  · rw [h0]; simp
  · rw [hj]; simp

/-! ## B. The status-ack watchdog

  tick  daemon.py `_status_poller`, one POLL_INTERVAL while connected
        (the `if not self.ble.connected: continue` tick changes nothing).
        outstanding = `_status_sent_at is not None`.
  ack   a `{"ack":"status"}` line reaches `_handle_ble` — the answer to a
        watchdog poll, or to the `{"cmd":"status"}` that `_send_resync` sends
        on every (re)connect.

MISSED_LIMIT = 2 and the de-escalation streak = 2, as in the code. -/

structure PState where
  esc : Nat           -- _ack_escalation
  clean : Nat         -- _clean_polls
  missed : Nat        -- _status_missed
  outstanding : Bool  -- _status_sent_at is not None
  pulses : Nat        -- pulse_reset calls
  reconnects : Nat    -- force_reconnect calls
  deriving DecidableEq, Repr

inductive PEv | tick | ack
  deriving DecidableEq, Repr

def pInit : PState := ⟨0, 0, 0, false, 0, 0⟩

/-- The escalation half of the poller, shared (daemon.py:808-843). -/
def pEscalate (s : PState) : PState :=
  if s.esc + 1 ≥ 2 then
    { s with missed := 0, outstanding := false, esc := 0, clean := 0, pulses := s.pulses + 1 }
  else
    { s with missed := 0, outstanding := false, esc := s.esc + 1, clean := 0,
             reconnects := s.reconnects + 1 }

/-- Current poller tick (daemon.py:806-846): a single miss leaves `_clean_polls` alone. -/
def pTickCur (s : PState) : PState :=
  if s.outstanding then
    if s.missed + 1 ≥ 2 then pEscalate s
    else { s with missed := s.missed + 1, outstanding := true }
  else { s with outstanding := true }

/-- Current ack branch (daemon.py:2489-2503): EVERY status ack counts as a clean
poll, the resync's included. -/
def pAckCur (s : PState) : PState :=
  { s with outstanding := false, missed := 0, clean := s.clean + 1,
           esc := if s.esc ≠ 0 ∧ s.clean + 1 ≥ 2 then 0 else s.esc }

def pStepCur (s : PState) : PEv → PState
  | .tick => pTickCur s
  | .ack => pAckCur s

/-- Fixed tick: a missed poll breaks the streak. -/
def pTickFix (s : PState) : PState :=
  if s.outstanding then
    if s.missed + 1 ≥ 2 then pEscalate s
    else { s with missed := s.missed + 1, outstanding := true, clean := 0 }
  else { s with outstanding := true }

/-- Fixed ack: only an ack that answers an outstanding watchdog poll counts. -/
def pAckFix (s : PState) : PState :=
  if s.outstanding then
    { s with outstanding := false, missed := 0, clean := s.clean + 1,
             esc := if s.esc ≠ 0 ∧ s.clean + 1 ≥ 2 then 0 else s.esc }
  else s

def pStepFix (s : PState) : PEv → PState
  | .tick => pTickFix s
  | .ack => pAckFix s

/-- One flap cycle after a reconnect: `r` resync/unsolicited status acks, then
optionally ONE answered watchdog poll, then the link goes dead long enough for
the watchdog to escalate (three poll intervals). -/
def cycle (c : Nat × Bool) : List PEv :=
  List.replicate c.1 .ack ++ (if c.2 then [.tick, .ack] else []) ++ [.tick, .tick, .tick]

def flap (cs : List (Nat × Bool)) : List PEv := cs.flatMap cycle

/-- The field pattern: resync ack + one poll ack after each reconnect, then deaf. -/
def fieldCycle : Nat × Bool := (1, true)

/-- Current code: the field flap resets the escalation on every cycle, so the
RTS pulse never fires, for ANY number of cycles. -/
theorem pCur_cycle (e p rc : Nat) (he : e ≤ 1) :
    run pStepCur ⟨e, 0, 0, false, p, rc⟩ (cycle fieldCycle) = ⟨1, 0, 0, false, p, rc + 1⟩ := by
  have : e = 0 ∨ e = 1 := by omega
  rcases this with h | h <;> subst h <;> rfl

theorem B_current_never_pulses (n : Nat) :
    (run pStepCur pInit (flap (List.replicate n fieldCycle))).pulses = 0 ∧
    (run pStepCur pInit (flap (List.replicate n fieldCycle))).reconnects = n := by
  suffices h : ∀ n e rc, e ≤ 1 →
      run pStepCur ⟨e, 0, 0, false, 0, rc⟩ (flap (List.replicate n fieldCycle)) =
        if n = 0 then ⟨e, 0, 0, false, 0, rc⟩ else ⟨1, 0, 0, false, 0, rc + n⟩ by
    have := h n 0 0 (by decide)
    unfold pInit
    rw [this]
    cases n <;> simp
  intro n
  induction n with
  | zero => intro e rc _; rfl
  | succ n ih =>
    intro e rc he
    simp only [flap, List.replicate_succ, List.flatMap_cons] at *
    rw [run_append, pCur_cycle e 0 rc he, ih 1 (rc + 1) (by decide)]
    cases n <;> simp <;> omega

/-- A concrete four-cycle instance, checked by evaluation. -/
theorem B_current_four_cycles :
    (run pStepCur pInit (flap [fieldCycle, fieldCycle, fieldCycle, fieldCycle])).pulses = 0 := by
  decide

theorem pFix_acks_noop (r : Nat) (s : PState) (h : s.outstanding = false) :
    run pStepFix s (List.replicate r .ack) = s := by
  induction r with
  | zero => rfl
  | succ r ih =>
    rw [List.replicate_succ, show (PEv.ack :: List.replicate r PEv.ack)
        = [PEv.ack] ++ List.replicate r PEv.ack from rfl, run_append]
    have : run pStepFix s [PEv.ack] = s := by simp [run, pStepFix, pAckFix, h]
    rw [this, ih]

/-- Fixed code: every flap cycle — any number of resync acks, at most one
answered poll — moves the escalation one step, and the second step pulses. -/
theorem pFix_cycle (c : Nat × Bool) (e p rc : Nat) (he : e ≤ 1) :
    run pStepFix ⟨e, 0, 0, false, p, rc⟩ (cycle c) =
      if e = 0 then ⟨1, 0, 0, false, p, rc + 1⟩ else ⟨0, 0, 0, false, p + 1, rc⟩ := by
  obtain ⟨r, b⟩ := c
  simp only [cycle]
  rw [run_append, run_append, pFix_acks_noop r _ rfl]
  have : e = 0 ∨ e = 1 := by omega
  rcases this with h | h <;> subst h <;> cases b <;> rfl

theorem pFix_flap (cs : List (Nat × Bool)) : ∀ (e p rc : Nat), e ≤ 1 →
    (run pStepFix ⟨e, 0, 0, false, p, rc⟩ (flap cs)).pulses = p + (e + cs.length) / 2 ∧
    (run pStepFix ⟨e, 0, 0, false, p, rc⟩ (flap cs)).esc = (e + cs.length) % 2 := by
  induction cs with
  | nil => intro e p rc he; simp [flap, run]; omega
  | cons c cs ih =>
    intro e p rc he
    simp only [flap, List.flatMap_cons, List.length_cons] at *
    rw [run_append, pFix_cycle c e p rc he]
    by_cases h0 : e = 0
    · subst h0
      simp only [ite_true]
      have := ih 1 p (rc + 1) (by decide)
      constructor <;> omega
    · have h1 : e = 1 := by omega
      subst h1
      simp only [show ¬ (1 = 0) from by decide, ite_false]
      have := ih 0 (p + 1) rc (by decide)
      constructor <;> omega

/-- B, fixed: for EVERY flap pattern (any list of cycles, any number of resync
acks per cycle, at most one answered watchdog poll per cycle), the RTS pulse
fires on every second escalation: `n` cycles give `n / 2` pulses. In
particular two consecutive flap cycles always pulse. -/
theorem B_fixed_flap_pulses (cs : List (Nat × Bool)) :
    (run pStepFix pInit (flap cs)).pulses = cs.length / 2 := by
  have := (pFix_flap cs 0 0 0 (by decide)).1
  simpa [pInit] using this

/-- B, fixed, for ANY single step from ANY state: the escalation only drops
without a pulse when an ack answers an outstanding watchdog poll and completes
a streak of two; and any missed poll zeroes the streak. -/
theorem B_fixed_deescalation_needs_streak (s : PState) (e : PEv)
    (hdrop : (pStepFix s e).esc < s.esc) (hnp : (pStepFix s e).pulses = s.pulses) :
    e = .ack ∧ s.outstanding = true ∧ s.clean + 1 ≥ 2 := by
  cases e with
  | tick =>
    exfalso
    simp only [pStepFix, pTickFix, pEscalate] at hdrop hnp
    by_cases ho : s.outstanding = true <;> by_cases hm : s.missed + 1 ≥ 2 <;>
      by_cases he : s.esc + 1 ≥ 2 <;> simp [ho, hm, he] at hdrop hnp <;> omega
  | ack =>
    simp only [pStepFix, pAckFix] at hdrop hnp
    split at hdrop
    · rename_i ho
      refine ⟨rfl, ho, ?_⟩
      simp only at hdrop
      split at hdrop
      · rename_i hc; exact hc.2
      · omega
    · omega

theorem B_fixed_miss_breaks_streak (s : PState) (h : s.outstanding = true) :
    (pStepFix s .tick).clean = 0 := by
  simp only [pStepFix, pTickFix, pEscalate, h, ite_true]
  split
  · split <;> rfl
  · rfl

/-! ## C. Request/ack matching (folder push)

Request ids stand for (ack type, `n`): the firmware's chunk ack carries `n`,
the file's cumulative byte count (firmware xfer.h `_xAck("chunk", true,
_xWritten)`), which the host can predict.

  send k    folder_push._send_expect: `await daemon.ble.send(payload)`
  reg k     Daemon.wait_for_ack / expect_ack: `self._ack_waiters.append(...)`
  ack k     the board's ack for request k reaches the router in _handle_ble
  expire k  the waiter for k times out (`asyncio.wait_for`), request k gives up

Current code sends first and registers second (folder_push.py:134-137), and the
router hands an ack to the oldest waiter of the same TYPE (daemon.py:2551-2556). -/

structure AState where
  sent : List Nat               -- requests on the wire, not answered or given up
  waiters : List Nat            -- registered waiters, oldest first
  delivered : List (Nat × Nat)  -- (waiter's request, ack's request)
  dropped : List Nat            -- acks dropped while their request was outstanding
  deriving DecidableEq, Repr

inductive AEv
  | send (k : Nat)
  | reg (k : Nat)
  | ack (k : Nat)
  | expire (k : Nat)

def aInit : AState := ⟨[], [], [], []⟩

def aExpire (s : AState) (k : Nat) : AState :=
  { s with waiters := s.waiters.filter (· ≠ k), sent := s.sent.filter (· ≠ k) }

def aStepCur (s : AState) : AEv → AState
  | .send k => { s with sent := k :: s.sent }
  | .reg k => { s with waiters := s.waiters ++ [k] }
  | .ack k =>
    match s.waiters with
    | w :: ws => { s with waiters := ws, sent := s.sent.filter (· ≠ w),
                          delivered := s.delivered ++ [(w, k)] }
    | [] => if k ∈ s.sent then { s with dropped := s.dropped ++ [k] } else s
  | .expire k => aExpire s k

/-- Fixed: `_send_expect` registers before it sends (a send is only issued for
a registered request), and the router matches the ack's `n`. -/
def aStepFix (s : AState) : AEv → AState
  | .send k => if k ∈ s.waiters then { s with sent := k :: s.sent } else s
  | .reg k => { s with waiters := s.waiters ++ [k] }
  | .ack k =>
    if k ∈ s.waiters then
      { s with waiters := s.waiters.filter (· ≠ k), sent := s.sent.filter (· ≠ k),
               delivered := s.delivered ++ [(k, k)] }
    else if k ∈ s.sent then { s with dropped := s.dropped ++ [k] } else s
  | .expire k => aExpire s k

/-- The ack lands between the send and the registration and is dropped. -/
theorem C_current_drops_ack :
    (run aStepCur aInit [.send 0, .ack 0, .reg 0]).dropped = [0] := by decide

/-- A late ack for a request that timed out satisfies the next request's waiter. -/
theorem C_current_stale_match :
    (run aStepCur aInit [.send 0, .reg 0, .expire 0, .send 1, .reg 1, .ack 0]).delivered
      = [(1, 0)] := by decide

def AInv (s : AState) : Prop :=
  (∀ k, k ∈ s.sent → k ∈ s.waiters) ∧ s.dropped = [] ∧ (∀ p ∈ s.delivered, p.1 = p.2)

theorem aStepFix_inv (s : AState) (e : AEv) (h : AInv s) : AInv (aStepFix s e) := by
  obtain ⟨h1, h2, h3⟩ := h
  cases e with
  | send k =>
    simp only [aStepFix]
    split
    · rename_i hk
      refine ⟨?_, h2, h3⟩
      intro j hj
      simp only [List.mem_cons] at hj
      rcases hj with hj | hj
      · subst hj; exact hk
      · exact h1 j hj
    · exact ⟨h1, h2, h3⟩
  | reg k =>
    refine ⟨?_, h2, h3⟩
    intro j hj
    simp only [aStepFix, List.mem_append]
    exact Or.inl (h1 j hj)
  | ack k =>
    simp only [aStepFix]
    split
    · refine ⟨?_, h2, ?_⟩
      · intro j hj
        simp only [List.mem_filter] at hj ⊢
        exact ⟨h1 j hj.1, hj.2⟩
      · intro p hp
        simp only [List.mem_append, List.mem_singleton] at hp
        rcases hp with hp | hp
        · exact h3 p hp
        · subst hp; rfl
    · rename_i hk
      split
      · rename_i hs; exact absurd (h1 k hs) hk
      · exact ⟨h1, h2, h3⟩
  | expire k =>
    refine ⟨?_, h2, h3⟩
    intro j hj
    simp only [aStepFix, aExpire, List.mem_filter] at hj ⊢
    exact ⟨h1 j hj.1, hj.2⟩

/-- C, fixed: for EVERY interleaving of sends, registrations, acks and
timeouts, no ack is dropped while its request is outstanding and every waiter
that receives an ack receives its own request's ack. -/
theorem C_fixed_acks_matched (es : List AEv) :
    (run aStepFix aInit es).dropped = [] ∧
    ∀ p ∈ (run aStepFix aInit es).delivered, p.1 = p.2 := by
  have h0 : AInv aInit := by
    refine ⟨?_, rfl, ?_⟩ <;> intro _ h <;> simp [aInit] at h
  have h := run_inv AInv aStepFix aStepFix_inv es aInit h0
  exact ⟨h.2.1, h.2.2⟩

/-! ## The two headline theorems -/

theorem current_violates :
    (run wStepCur wInit aTrace).writes.length = 2 ∧
    (∀ n, (run pStepCur pInit (flap (List.replicate n fieldCycle))).pulses = 0) ∧
    (run aStepCur aInit [.send 0, .ack 0, .reg 0]).dropped = [0] ∧
    (run aStepCur aInit [.send 0, .reg 0, .expire 0, .send 1, .reg 1, .ack 0]).delivered
      = [(1, 0)] :=
  ⟨A_current_two_writes, fun n => (B_current_never_pulses n).1,
   C_current_drops_ack, C_current_stale_match⟩

theorem fixed_invariant :
    (∀ es, (run wStepFix wInit es).writes.length ≤ 1) ∧
    (∀ cs, (run pStepFix pInit (flap cs)).pulses = cs.length / 2) ∧
    (∀ s e, (pStepFix s e).esc < s.esc → (pStepFix s e).pulses = s.pulses →
       e = .ack ∧ s.outstanding = true ∧ s.clean + 1 ≥ 2) ∧
    (∀ es, (run aStepFix aInit es).dropped = [] ∧
       ∀ p ∈ (run aStepFix aInit es).delivered, p.1 = p.2) :=
  ⟨A_fixed_single_writer, B_fixed_flap_pulses, B_fixed_deescalation_needs_streak,
   C_fixed_acks_matched⟩

end Eve.Serial
