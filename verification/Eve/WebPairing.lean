/-
  Eve.WebPairing — how a phone, a laptop or the Mac's own browser gets into eve lite, and how it is put out
  again (eve lite, 2026-10-13): the pairing window behind the QR code and the 6-digit code, the Mac's one-time
  login code, the device tokens kept only as hashes, and revoke.

  Written with the code it models, so the honest shape is `code_invariant`: the properties proved of the code as
  written, over every trace. There is no earlier version to put a `current_violates` on. `naive_violates` is the
  positive control: each of the four mechanisms, taken out alone, breaks its own property on a concrete trace.

  Code modelled, bridge/src/eve_bridge/web_auth.py (WP-4; it was not committed while it was modelled, so the line
  numbers are those of the working tree of 2026-10-13):
  - `PairingWindow` (300-375). `open` (313-325) draws a 128-bit secret and a 6-digit code, keeps only their hashes,
    sets `_until` to now + ttl (PAIR_SECS, 300) and no tries, and replaces any window; `close` (327-330); `_live`
    (368-371): open until the clock reaches `_until`. `pair` (343-366): refused while not live; a hit is the secret
    given alone or the code given alone (`_code_digits` reads "123 456" as 123456), compared in constant time, and
    closes the window (`_shut`); anything else (both given, neither, malformed, a miss) is one wrong try, and the
    PAIR_TRIES-th (5) closes the window.
  - `MacLogin` (387-433). `issue` (396-406) prunes, drops the oldest code while MAX_LOGINS (16) wait, and keeps the
    new code's hash with a deadline now + LOGIN_SECS (120); `consume` (408-420) prunes, then answers true once for
    a code it issued, unused and before its deadline; `_prune` (431-433) drops a code LOGIN_KEEP_SECS (60) past its
    deadline.
  - `Devices` (157-287). `add` (169-187) draws a 256-bit token and an 8-hex id no stored device has (it draws again
    while taken), and stores the token's SHA-256 and when it was seen, or pairs nothing when the list cannot be
    written; `check` (189-212) takes the last row whose hash is the token's, refuses it once unseen for MAX_AGE (400
    days), and marks it seen at most every SEEN_SECS; `revoke` (214-226) drops one id, or every device for "all",
    and returns their ids; `_kept` (270-278), on every write, also forgets a device unseen for MAX_AGE and, past
    MAX_DEVICES (64), those seen longest ago (`forget` here).
  - The composition the spec fixes for lite_web.py and lite.py (docs/plans/eve-lite.md M4, M11): a device is added
    after a successful `pair` (`POST /api/web/pair`) or `consume` (`POST /api/mac/login`) and after nothing else,
    and `add` may fail to save (`saved`); revoking closes the live peers of every id `revoke` returned
    (`RtcHub.close_device`); every request with a cookie, and `POST /api/web/rtc`, runs only on a token `check`
    accepts, and a device has one live peer, the newest (M6).

  The properties, over every trace of opens, closes, pair attempts, login codes, requests, peers, revokes, forgets
  and clock ticks:
  - (P1) single use: a pairing window lets at most one pair through; a login code logs in at most once;
  - (P2) expiry: a window pairs only before its deadline (opened + 300 s), a login code logs in only before its own
    (issued + 120 s), and a cookie checks only while its device was seen within MAX_AGE;
  - (P3) bounded attempts: at most PAIR_TRIES (5) tries ever reach one window, the one that pairs included, so a
    window that took five wrong tries never pairs;
  - (P4) revoke: a revoked device is out of the store and has no live peer, for good;
  - (P5) authentication: a cookie checks only as a stored, unrevoked device that a successful pair or Mac login
    minted with a token of the cookie's hash; a success mints at most one device.
  `fresh_revoked_token_dies` adds the corollary for a hash with no collisions and tokens drawn fresh: a revoked
  device's own token checks as nothing at all.

  Abstractions, stated:
  - Values. A secret, a code, a token and an id are numbers; each event carries what `secrets` drew, so every
    theorem holds for every draw. A code is its digits (`_code_digits`). The hash is any function `H` (SHA-256); the
    window's and the login codes' own hashes are taken as injective. Nothing is probabilistic: P3 bounds the tries a
    window ever takes (five), and the chance that five tries hit a 6-digit code (5 in 10^6 per window) is
    arithmetic outside the model.
  - Who opens a window. Only `POST /api/web/pair/open` opens one, on the local listener, for an `eve_mac` cookie;
    the LAN listener has no such route (M4's table). So `openWin` is the owner's act, and P3 bounds a guesser on the
    LAN to five tries per window the owner opened. That routing is lite_web.py's, tested there.
  - Devices are told apart by a ghost serial, the index of the mint that made them: ids are random and only unique
    among stored devices, so an id revoked once can be drawn again for a new device, which is not revoked.
  - Forgetting is not revoking. `_kept` forgets a device unseen for MAX_AGE or past MAX_DEVICES; its cookie stops
    checking (`forget_is_not_revoke`), but its live peer, if it has one, is closed only when it hangs up: the spec
    closes peers on revoke alone. `forget` may drop any devices here (`_kept` picks the ones seen longest ago). A
    revoke whose write fails drops the rows in memory and raises (OSError) before the caller can close a peer: that
    is a `forget` of those ids too. A restart, which reads the file back (so a revoke that failed to save is undone)
    and drops every peer, is not modelled.
  - Time. One clock in whole seconds that only moves on (`tick`). The code reads `time.monotonic()` for windows and
    codes and `time.time()` for devices; a wall clock set back makes a device look seen more recently, which `check`
    accepts as the model's truncated subtraction does.
  - A login code issued twice (a 128-bit collision) gets a new slot here; the dict keeps the first one's. Pruning
    and the 16-code cap only drop codes, as here. The peer cap of RtcHub only closes peers, which every property
    survives; it is not modelled. Cookies, Host and Origin checks, TLS and the 0600 file modes are lite_web.py's and
    lite_tls.py's, tested there.
-/
namespace Eve.WebPairing

/-! ## Constants (web_auth.py:40-48) -/

def PAIR_SECS : Nat := 300
def PAIR_TRIES : Nat := 5
def LOGIN_SECS : Nat := 120
def LOGIN_KEEP_SECS : Nat := 60
def MAX_LOGINS : Nat := 16
def MAX_AGE : Nat := 34560000
def SEEN_SECS : Nat := 600

/-! ## State -/

/-- A pairing window: its secret and its code (their hashes, in the code), its deadline, its wrong tries, whether a
pair hit it and whether `close` shut it. `wins` and `wonAt` are ghosts: how many pairs it let through, and when. -/
structure Win where
  secret : Nat
  code : Nat
  deadline : Nat
  tries : Nat := 0
  used : Bool := false
  shut : Bool := false
  wins : Nat := 0
  wonAt : Nat := 0
  deriving DecidableEq, Repr

/-- One Mac login code as `MacLogin` keeps it; `uses` and `usedAt` are ghosts. -/
structure Login where
  code : Nat
  deadline : Nat
  used : Bool := false
  uses : Nat := 0
  usedAt : Nat := 0
  deriving DecidableEq, Repr

/-- One row of web-devices.json: the id, the token's hash, when it was last seen; `serial` is a ghost, the index of
the mint that made it. The label, the host and the created time are not modelled. -/
structure Dev where
  id : Nat
  hash : Nat
  seen : Nat
  serial : Nat
  deriving DecidableEq, Repr

structure St where
  now : Nat := 0
  win : Option Win := none         -- the pairing window, once one was opened
  past : List Win := []            -- ghost: every window a newer one replaced
  logins : List Login := []        -- MacLogin's codes, newest first
  dropped : List Login := []       -- ghost: every code record pruned, dropped for the cap, or issued over
  devs : List Dev := []            -- web-devices.json, oldest first
  peers : List (Nat × Nat) := []   -- each live peer: its device's id and (ghost) serial
  minted : List (Nat × Nat) := []  -- ghost: the id and the token of each mint, by serial
  revoked : List Nat := []         -- ghost: the serials of every device a revoke removed
  pairs : Nat := 0                 -- ghost: pairs the window let through
  macLogins : Nat := 0             -- ghost: Mac logins
  deriving Repr

/-! ## Events -/

inductive Ev where
  /-- The clock moves on `dt` seconds. -/
  | tick (dt : Nat)
  /-- `POST /api/web/pair/open`: `PairingWindow.open()` drew this secret and this code. -/
  | openWin (secret code : Nat)
  /-- `PairingWindow.close()`. -/
  | closeWin
  /-- `POST /api/web/pair {secret}` or `{code}`: `pair(...)`; on a hit, `Devices.add` with these id draws and this
  token, which saves or raises (`saved`). -/
  | pair (secret code : Option Nat) (draws : List Nat) (token : Nat) (saved : Bool)
  /-- `{"evt":"web_login"}` over the IPC socket: `MacLogin.issue()` drew `code`. -/
  | issue (code : Nat)
  /-- `POST /api/mac/login {code}`: `consume(code)`; on true, `Devices.add` as for a pair. -/
  | consume (code : Nat) (draws : List Nat) (token : Nat) (saved : Bool)
  /-- `POST /api/web/devices/revoke` or `{"evt":"revoke"}`: one id, or `none` for "all". -/
  | revoke (target : Option Nat)
  /-- Any request with a cookie holding `token`: `Devices.check`. -/
  | request (token : Nat)
  /-- `POST /api/web/rtc` with a cookie holding `token`: `check`, then that device's one peer, the newest. -/
  | connect (token : Nat)
  /-- The peer of device `id` goes away. -/
  | hangup (id : Nat)
  /-- `_kept` forgets these devices on a write (unseen for MAX_AGE, or past MAX_DEVICES), or a revoke failed to save. -/
  | forget (ids : List Nat)
  deriving Repr

/-! ## The machine -/

/-- Which checks the code makes. `impl` is the code; each field off alone is a positive control. -/
structure Ver where
  singleUse : Bool := true       -- a hit shuts the window; a used login code is refused
  expires : Bool := true         -- the deadlines and MAX_AGE are checked
  lockout : Bool := true         -- the PAIR_TRIES-th wrong try shuts the window
  closePeers : Bool := true      -- revoke closes the revoked devices' live peers

def impl : Ver := {}

/-- `_live` (368-371), and the shut a hit or the fifth wrong try makes (358-359, 362-364). -/
def Win.live (v : Ver) (w : Win) (now : Nat) : Bool :=
  !w.shut && (!v.singleUse || !w.used) && (!v.lockout || decide (w.tries < PAIR_TRIES)) &&
    (!v.expires || decide (now < w.deadline))

/-- `pair`'s hit (349-358): the secret given alone, or the code given alone. -/
def Win.hits (w : Win) : Option Nat → Option Nat → Bool
  | some s, none => s == w.secret
  | none, some c => c == w.code
  | _, _ => false

/-- The window after a hit at `now`. -/
def Win.use (w : Win) (now : Nat) : Win := { w with used := true, wins := w.wins + 1, wonAt := now }

/-- The window after a wrong try. -/
def Win.miss (w : Win) : Win := { w with tries := w.tries + 1 }

/-- `_prune` (431-433): a code stays until LOGIN_KEEP_SECS past its deadline. -/
def keepLogin (now : Nat) (l : Login) : Bool := decide (now < l.deadline + LOGIN_KEEP_SECS)

/-- `consume` on the pruned codes (415-418): the code's record, unused and before its deadline, is used now
(`some` the new records); anything else changes nothing (`none`). -/
def useCode (v : Ver) (code now : Nat) : List Login → Option (List Login)
  | [] => none
  | l :: ls =>
      if l.code = code then
        if (v.singleUse && l.used) || (v.expires && !decide (now < l.deadline)) then none
        else some ({ l with used := true, uses := l.uses + 1, usedAt := now } :: ls)
      else (useCode v code now ls).map (l :: ·)

/-- `add`'s id (176-179): the first draw no stored device has. A draw sequence that never ends on a free id is not a
run of the loop; it gets a free id all the same. -/
def pickId (devs : List Dev) : List Nat → Nat
  | [] => (devs.map (·.id)).foldr max 0 + 1
  | i :: is => if (devs.map (·.id)).contains i then pickId devs is else i

/-- `Devices.add` (169-187): the new row, and the mint's ghost record. -/
def St.add (H : Nat → Nat) (s : St) (draws : List Nat) (token : Nat) : St :=
  { s with devs := s.devs ++ [{ id := pickId s.devs draws, hash := H token, seen := s.now, serial := s.minted.length }],
           minted := s.minted ++ [(pickId s.devs draws, token)] }

/-- The row `check` finds (198-201): the last one whose hash is the token's. -/
def lookup (H : Nat → Nat) (devs : List Dev) (token : Nat) : Option Dev :=
  devs.reverse.find? (fun d => d.hash == H token)

/-- `Devices.check(token)` (189-212): that row, unless unseen for MAX_AGE (202). -/
def check (v : Ver) (H : Nat → Nat) (now : Nat) (devs : List Dev) (token : Nat) : Option Dev :=
  match lookup H devs token with
  | some d => if v.expires && decide (MAX_AGE < now - d.seen) then none else some d
  | none => none

/-- `check` marks the device it found seen, at most every SEEN_SECS (204-205). -/
def seenNow (now : Nat) (d x : Dev) : Dev :=
  if x.serial == d.serial && decide (SEEN_SECS ≤ now - x.seen) then { x with seen := now } else x

/-- The state after a pair hit window `w` (358-361), before `Devices.add`. -/
def St.paired (s : St) (w : Win) : St := { s with win := some (w.use s.now), pairs := s.pairs + 1 }

/-- The state after `consume` pruned the codes and used one (`ls`, 414-419), before `Devices.add`. -/
def St.consumed (s : St) (ls : List Login) : St :=
  { s with logins := ls, dropped := s.logins.filter (fun l => !keepLogin s.now l) ++ s.dropped,
           macLogins := s.macLogins + 1 }

/-- The rows `revoke(target)` drops (221-223): the one id, or every row for "all". -/
def goneRows (devs : List Dev) (target : Option Nat) : List Dev :=
  devs.filter (fun g => match target with | some i => g.id == i | none => true)

def step (v : Ver) (H : Nat → Nat) (s : St) : Ev → St
  | .tick dt => { s with now := s.now + dt }
  | .openWin sec code =>
      { s with win := some { secret := sec, code := code, deadline := s.now + PAIR_SECS },
               past := s.win.toList ++ s.past }
  | .closeWin => { s with win := s.win.map (fun w => { w with shut := true }) }
  | .pair sec code draws token saved =>
      match s.win with
      | none => s
      | some w =>
          if !w.live v s.now then s
          else if w.hits sec code then
            if saved then (s.paired w).add H draws token else s.paired w
          else { s with win := some w.miss }
  | .issue code =>
      { s with logins := { code := code, deadline := s.now + LOGIN_SECS } ::
                 ((s.logins.filter (keepLogin s.now)).filter (fun l => l.code != code)).take (MAX_LOGINS - 1),
               dropped := s.logins.filter (fun l => !keepLogin s.now l) ++
                 (s.logins.filter (keepLogin s.now)).filter (fun l => l.code == code) ++
                 ((s.logins.filter (keepLogin s.now)).filter (fun l => l.code != code)).drop (MAX_LOGINS - 1) ++
                 s.dropped }
  | .consume code draws token saved =>
      match useCode v code s.now (s.logins.filter (keepLogin s.now)) with
      | none => { s with logins := s.logins.filter (keepLogin s.now),
                         dropped := s.logins.filter (fun l => !keepLogin s.now l) ++ s.dropped }
      | some ls => if saved then (s.consumed ls).add H draws token else s.consumed ls
  | .revoke target =>
      { s with devs := s.devs.filter (fun d => !((goneRows s.devs target).map (·.id)).contains d.id),
               peers := if v.closePeers then s.peers.filter (fun p => !((goneRows s.devs target).map (·.id)).contains p.1)
                 else s.peers,
               revoked := s.revoked ++ (goneRows s.devs target).map (·.serial) }
  | .request token =>
      match check v H s.now s.devs token with
      | some d => { s with devs := s.devs.map (seenNow s.now d) }
      | none => s
  | .connect token =>
      match check v H s.now s.devs token with
      | some d => { s with devs := s.devs.map (seenNow s.now d),
                           peers := (d.id, d.serial) :: s.peers.filter (fun p => p.1 != d.id) }
      | none => s
  | .hangup i => { s with peers := s.peers.filter (fun p => p.1 != i) }
  | .forget ids => { s with devs := s.devs.filter (fun d => !ids.contains d.id) }

def run (v : Ver) (H : Nat → Nat) (evs : List Ev) : St := evs.foldl (step v H) {}

/-! ## The properties -/

/-- P1-P3 for one window, with the fact the induction needs (an unused window let nothing through). -/
def WinOk (w : Win) : Prop :=
  w.tries + w.wins ≤ PAIR_TRIES ∧ w.wins ≤ 1 ∧ (w.wins = 1 → w.used = true ∧ w.wonAt < w.deadline) ∧
    (w.used = false → w.wins = 0)

/-- P1, P2 for one login code. -/
def LoginOk (l : Login) : Prop :=
  l.uses ≤ 1 ∧ (l.uses = 1 → l.used = true ∧ l.usedAt < l.deadline) ∧ (l.used = false → l.uses = 0)

def Inv (H : Nat → Nat) (s : St) : Prop :=
  (∀ w ∈ s.win.toList ++ s.past, WinOk w) ∧
  (∀ l ∈ s.logins ++ s.dropped, LoginOk l) ∧
  (∀ d ∈ s.devs, ∃ t, s.minted[d.serial]? = some (d.id, t) ∧ d.hash = H t) ∧
  (∀ r ∈ s.revoked, r < s.minted.length ∧ ∀ d ∈ s.devs, d.serial ≠ r) ∧
  (∀ p ∈ s.peers, (∃ t, s.minted[p.2]? = some (p.1, t)) ∧ p.2 ∉ s.revoked) ∧
  s.minted.length ≤ s.pairs + s.macLogins

theorem inv_init (H : Nat → Nat) : Inv H {} := by
  refine ⟨?_, ?_, ?_, ?_, ?_, ?_⟩ <;> simp

/-! ## Proofs -/

theorem lt_of_getElem_some {α : Type} (l : List α) (i : Nat) (a : α) (h : l[i]? = some a) : i < l.length :=
  (List.getElem?_eq_some_iff.mp h).1

/-- `Devices.add` keeps the device half of the invariant and mints one. -/
theorem add_inv (H : Nat → Nat) (s : St) (draws : List Nat) (token : Nat)
    (hd : ∀ d ∈ s.devs, ∃ t, s.minted[d.serial]? = some (d.id, t) ∧ d.hash = H t)
    (hr : ∀ r ∈ s.revoked, r < s.minted.length ∧ ∀ d ∈ s.devs, d.serial ≠ r)
    (hp : ∀ p ∈ s.peers, (∃ t, s.minted[p.2]? = some (p.1, t)) ∧ p.2 ∉ s.revoked) :
    (∀ d ∈ (s.add H draws token).devs, ∃ t, (s.add H draws token).minted[d.serial]? = some (d.id, t) ∧ d.hash = H t) ∧
    (∀ r ∈ (s.add H draws token).revoked,
      r < (s.add H draws token).minted.length ∧ ∀ d ∈ (s.add H draws token).devs, d.serial ≠ r) ∧
    (∀ p ∈ (s.add H draws token).peers,
      (∃ t, (s.add H draws token).minted[p.2]? = some (p.1, t)) ∧ p.2 ∉ (s.add H draws token).revoked) ∧
    (s.add H draws token).minted.length = s.minted.length + 1 := by
  simp only [St.add]
  refine ⟨?_, ?_, ?_, by simp⟩
  · intro d hd'
    rcases List.mem_append.mp hd' with h | h
    · obtain ⟨t, ht, hh⟩ := hd d h
      exact ⟨t, by rw [List.getElem?_append_left (lt_of_getElem_some _ _ _ ht)]; exact ht, hh⟩
    · simp only [List.mem_singleton] at h; subst h
      exact ⟨token, by simp, rfl⟩
  · intro r hr'
    obtain ⟨hlt, hne⟩ := hr r hr'
    refine ⟨by simp; omega, ?_⟩
    intro d hd' heq
    rcases List.mem_append.mp hd' with h | h
    · exact hne d h heq
    · simp only [List.mem_singleton] at h; subst h; simp at heq; omega
  · intro p hp'
    obtain ⟨⟨t, ht⟩, hnr⟩ := hp p hp'
    exact ⟨⟨t, by rw [List.getElem?_append_left (lt_of_getElem_some _ _ _ ht)]; exact ht⟩, hnr⟩

/-- A login record `useCode` used was unused and before its deadline: every record after is `LoginOk`. -/
theorem useCode_ok (code now : Nat) (ls ls' : List Login) (h : useCode impl code now ls = some ls')
    (hok : ∀ l ∈ ls, LoginOk l) : ∀ l ∈ ls', LoginOk l := by
  induction ls generalizing ls' with
  | nil => simp [useCode] at h
  | cons l rest ih =>
      simp only [useCode] at h
      by_cases hc : l.code = code
      · simp only [hc, ↓reduceIte] at h
        by_cases hb : (impl.singleUse && l.used) || (impl.expires && !decide (now < l.deadline))
        · simp [hb] at h
        · simp only [hb, Bool.false_eq_true, ↓reduceIte, Option.some.injEq] at h
          subst h
          have hl := hok l (by simp)
          have hu : l.used = false := by
            cases hx : l.used
            · rfl
            · simp [impl, hx] at hb
          have hdl : now < l.deadline := by
            by_cases hc' : now < l.deadline
            · exact hc'
            · simp [impl, hc'] at hb
          have h0 := hl.2.2 hu
          intro x hx
          rcases List.mem_cons.mp hx with hx | hx
          · subst hx
            refine ⟨by simp [h0], ?_, by simp⟩
            intro _; exact ⟨rfl, hdl⟩
          · exact hok x (List.mem_cons_of_mem _ hx)
      · simp only [hc, ↓reduceIte] at h
        cases hr : useCode impl code now rest with
        | none => simp [hr] at h
        | some r =>
            simp only [hr, Option.map_some, Option.some.injEq] at h
            subst h
            have ih' := ih r hr (fun x hx => hok x (List.mem_cons_of_mem _ hx))
            intro x hx
            rcases List.mem_cons.mp hx with hx | hx
            · subst hx; exact hok _ (by simp)
            · exact ih' x hx

theorem lookup_mem (H : Nat → Nat) (devs : List Dev) (t : Nat) (d : Dev) (h : lookup H devs t = some d) :
    d ∈ devs ∧ d.hash = H t := by
  unfold lookup at h
  exact ⟨List.mem_reverse.mp (List.mem_of_find?_eq_some h), by simpa using List.find?_some h⟩

theorem check_mem (H : Nat → Nat) (now : Nat) (devs : List Dev) (t : Nat) (d : Dev)
    (h : check impl H now devs t = some d) : d ∈ devs ∧ d.hash = H t ∧ now - d.seen ≤ MAX_AGE := by
  unfold check at h
  cases hl : lookup H devs t with
  | none => simp [hl] at h
  | some e =>
      simp only [hl] at h
      by_cases ha : MAX_AGE < now - e.seen
      · simp [impl, ha] at h
      · have he : e = d := by simpa [impl, ha] using h
        subst he
        obtain ⟨hm, hh⟩ := lookup_mem H devs t _ hl
        exact ⟨hm, hh, by omega⟩

/-- `seenNow` changes a row's `seen` only. -/
theorem seenNow_keeps (now : Nat) (d x : Dev) :
    (seenNow now d x).id = x.id ∧ (seenNow now d x).hash = x.hash ∧ (seenNow now d x).serial = x.serial := by
  unfold seenNow; split <;> simp

/-- Marking a device seen keeps the device half of the invariant. -/
theorem seen_inv (H : Nat → Nat) (s : St) (now : Nat) (d : Dev)
    (hd : ∀ d ∈ s.devs, ∃ t, s.minted[d.serial]? = some (d.id, t) ∧ d.hash = H t)
    (hr : ∀ r ∈ s.revoked, r < s.minted.length ∧ ∀ d ∈ s.devs, d.serial ≠ r) :
    (∀ x ∈ s.devs.map (seenNow now d), ∃ t, s.minted[x.serial]? = some (x.id, t) ∧ x.hash = H t) ∧
    (∀ r ∈ s.revoked, r < s.minted.length ∧ ∀ x ∈ s.devs.map (seenNow now d), x.serial ≠ r) := by
  refine ⟨?_, ?_⟩
  · intro x hx
    obtain ⟨y, hy, rfl⟩ := List.mem_map.mp hx
    obtain ⟨h1, h2, h3⟩ := seenNow_keeps now d y
    rw [h1, h2, h3]; exact hd y hy
  · intro r hr'
    refine ⟨(hr r hr').1, ?_⟩
    intro x hx
    obtain ⟨y, hy, rfl⟩ := List.mem_map.mp hx
    rw [(seenNow_keeps now d y).2.2]; exact (hr r hr').2 y hy

/-- A device `revoke` dropped shares its id with no device it kept: two stored devices with one serial are one mint,
so one id. -/
theorem gone_id (H : Nat → Nat) (s : St) (target : Option Nat)
    (hd : ∀ d ∈ s.devs, ∃ t, s.minted[d.serial]? = some (d.id, t) ∧ d.hash = H t)
    (g : Dev) (hg : g ∈ goneRows s.devs target) (i : Nat) (t : Nat) (hm : s.minted[g.serial]? = some (i, t)) :
    i ∈ (goneRows s.devs target).map (·.id) := by
  obtain ⟨tg, htg, _⟩ := hd g (List.mem_filter.mp hg).1
  rw [hm] at htg
  simp only [Option.some.injEq, Prod.mk.injEq] at htg
  exact List.mem_map.mpr ⟨g, hg, htg.1.symm⟩

theorem inv_step (H : Nat → Nat) (s : St) (e : Ev) (h : Inv H s) : Inv H (step impl H s e) := by
  obtain ⟨hw, hl, hd, hr, hp, hc⟩ := h
  cases e with
  | tick dt => exact ⟨hw, hl, hd, hr, hp, hc⟩
  | openWin sec code =>
      refine ⟨?_, hl, hd, hr, hp, hc⟩
      intro w hw'
      simp only [step, Option.toList_some, List.cons_append, List.nil_append, List.mem_cons] at hw'
      rcases hw' with hw' | hw'
      · subst hw'; exact ⟨by simp [PAIR_TRIES], by simp, by simp, by simp⟩
      · exact hw w hw'
  | closeWin =>
      refine ⟨?_, hl, hd, hr, hp, hc⟩
      intro w hw'
      simp only [step, List.mem_append] at hw'
      rcases hw' with hw' | hw'
      · cases hwin : s.win with
        | none => simp [hwin] at hw'
        | some x =>
            simp only [hwin, Option.map_some, Option.toList_some, List.mem_singleton] at hw'
            subst hw'
            obtain ⟨h1, h2, h3, h4⟩ := hw x (by simp [hwin])
            exact ⟨h1, h2, h3, h4⟩
      · exact hw w (List.mem_append_right _ hw')
  | pair sec code draws token saved =>
      simp only [step]
      cases hwin : s.win with
      | none => exact ⟨hw, hl, hd, hr, hp, hc⟩
      | some w =>
          have hwok : WinOk w := hw w (by simp [hwin])
          by_cases ho : w.live impl s.now = true
          · simp only [ho, Bool.not_true, Bool.false_eq_true, ↓reduceIte]
            simp only [Win.live, impl, Bool.not_true, Bool.false_or, Bool.and_eq_true, Bool.not_eq_true',
              decide_eq_true_eq] at ho
            obtain ⟨⟨⟨_, hu⟩, hlt⟩, hdl⟩ := ho
            have h0 := hwok.2.2.2 hu
            have huse : ∀ x ∈ (some (w.use s.now)).toList ++ s.past, WinOk x := by
              intro x hx
              simp only [Option.toList_some, List.cons_append, List.nil_append, List.mem_cons] at hx
              rcases hx with hx | hx
              · subst hx
                refine ⟨by simp [Win.use]; omega, by simp [Win.use, h0], ?_, by simp [Win.use]⟩
                intro _; exact ⟨rfl, hdl⟩
              · exact hw x (by simp [hwin, hx])
            by_cases hh : w.hits sec code = true
            · simp only [hh, ↓reduceIte]
              cases saved with
              | true =>
                  simp only [↓reduceIte]
                  obtain ⟨ad, ar, ap, al⟩ := add_inv H (s.paired w) draws token hd hr hp
                  refine ⟨huse, hl, ad, ar, ap, ?_⟩
                  rw [al]; simp only [St.add, St.paired]; omega
              | false =>
                  simp only [Bool.false_eq_true, ↓reduceIte]
                  exact ⟨huse, hl, hd, hr, hp, by simp only [St.paired]; omega⟩
            · simp only [hh, Bool.false_eq_true, ↓reduceIte]
              refine ⟨?_, hl, hd, hr, hp, hc⟩
              intro x hx
              simp only [Option.toList_some, List.cons_append, List.nil_append, List.mem_cons] at hx
              rcases hx with hx | hx
              · subst hx
                refine ⟨by simp [Win.miss, h0]; omega, by simp [Win.miss, h0], by simp [Win.miss, h0],
                  fun _ => by simp [Win.miss, h0]⟩
              · exact hw x (by simp [hwin, hx])
          · simp only [Bool.not_eq_true] at ho
            simp only [ho, Bool.not_false, ↓reduceIte]
            exact ⟨hw, hl, hd, hr, hp, hc⟩
  | issue code =>
      refine ⟨hw, ?_, hd, hr, hp, hc⟩
      intro x hx
      simp only [step, List.cons_append, List.mem_cons, List.mem_append] at hx
      rcases hx with hx | hx | ((hx | hx) | hx) | hx
      · subst hx; exact ⟨by simp, by simp, by simp⟩
      · have := List.mem_of_mem_take hx
        exact hl x (List.mem_append_left _ (List.mem_filter.mp (List.mem_filter.mp this).1).1)
      · exact hl x (List.mem_append_left _ (List.mem_filter.mp hx).1)
      · exact hl x (List.mem_append_left _ (List.mem_filter.mp (List.mem_filter.mp hx).1).1)
      · have := List.mem_of_mem_drop hx
        exact hl x (List.mem_append_left _ (List.mem_filter.mp (List.mem_filter.mp this).1).1)
      · exact hl x (List.mem_append_right _ hx)
  | consume code draws token saved =>
      have hpruned : ∀ x ∈ s.logins.filter (keepLogin s.now), LoginOk x :=
        fun x hx => hl x (List.mem_append_left _ (List.mem_filter.mp hx).1)
      have hdrop : ∀ x ∈ s.logins.filter (fun l => !keepLogin s.now l) ++ s.dropped, LoginOk x := by
        intro x hx
        rcases List.mem_append.mp hx with hx | hx
        · exact hl x (List.mem_append_left _ (List.mem_filter.mp hx).1)
        · exact hl x (List.mem_append_right _ hx)
      simp only [step]
      cases hu : useCode impl code s.now (s.logins.filter (keepLogin s.now)) with
      | none =>
          refine ⟨hw, ?_, hd, hr, hp, hc⟩
          intro x hx
          rcases List.mem_append.mp hx with hx | hx
          · exact hpruned x hx
          · exact hdrop x hx
      | some ls =>
          have hls := useCode_ok code s.now _ ls hu hpruned
          have hlog : ∀ x ∈ ls ++ (s.logins.filter (fun l => !keepLogin s.now l) ++ s.dropped), LoginOk x := by
            intro x hx
            rcases List.mem_append.mp hx with hx | hx
            · exact hls x hx
            · exact hdrop x hx
          cases saved with
          | true =>
              simp only [↓reduceIte]
              obtain ⟨ad, ar, ap, al⟩ := add_inv H (s.consumed ls) draws token hd hr hp
              refine ⟨hw, hlog, ad, ar, ap, ?_⟩
              rw [al]; simp only [St.add, St.consumed]; omega
          | false =>
              simp only [Bool.false_eq_true, ↓reduceIte]
              exact ⟨hw, hlog, hd, hr, hp, by simp only [St.consumed]; omega⟩
  | revoke target =>
      refine ⟨hw, hl, ?_, ?_, ?_, hc⟩
      · intro d hd'
        simp only [step] at hd'
        exact hd d (List.mem_filter.mp hd').1
      · intro r hr'
        simp only [step, List.mem_append] at hr'
        rcases hr' with hr' | hr'
        · obtain ⟨hlt, hne⟩ := hr r hr'
          refine ⟨hlt, fun d hd' => hne d ?_⟩
          simp only [step] at hd'
          exact (List.mem_filter.mp hd').1
        · obtain ⟨g, hg, rfl⟩ := List.mem_map.mp hr'
          obtain ⟨tg, htg, _⟩ := hd g (List.mem_filter.mp hg).1
          refine ⟨lt_of_getElem_some _ _ _ htg, ?_⟩
          intro d hd' heq
          simp only [step] at hd'
          obtain ⟨hdm, hng⟩ := List.mem_filter.mp hd'
          obtain ⟨td, htd, _⟩ := hd d hdm
          rw [heq] at htd
          have := gone_id H s target hd g hg d.id td htd
          simp [this] at hng
      · intro p hp'
        simp only [step, impl, ↓reduceIte, List.mem_filter] at hp'
        obtain ⟨hp', hng⟩ := hp'
        obtain ⟨⟨t, ht⟩, hnr⟩ := hp p hp'
        refine ⟨⟨t, ht⟩, ?_⟩
        simp only [step, List.mem_append, not_or]
        refine ⟨hnr, ?_⟩
        intro hin
        obtain ⟨g, hg, hgs⟩ := List.mem_map.mp hin
        rw [← hgs] at ht
        have := gone_id H s target hd g hg p.1 t ht
        simp [this] at hng
  | request token =>
      simp only [step]
      cases hk : check impl H s.now s.devs token with
      | none => exact ⟨hw, hl, hd, hr, hp, hc⟩
      | some d =>
          obtain ⟨sd, sr⟩ := seen_inv H s s.now d hd hr
          exact ⟨hw, hl, sd, sr, hp, hc⟩
  | connect token =>
      simp only [step]
      cases hk : check impl H s.now s.devs token with
      | none => exact ⟨hw, hl, hd, hr, hp, hc⟩
      | some d =>
          obtain ⟨sd, sr⟩ := seen_inv H s s.now d hd hr
          refine ⟨hw, hl, sd, sr, ?_, hc⟩
          intro p hp'
          rcases List.mem_cons.mp hp' with hp' | hp'
          · subst hp'
            have hm := (check_mem H s.now s.devs token d hk).1
            obtain ⟨t, ht, _⟩ := hd d hm
            exact ⟨⟨t, ht⟩, fun hin => (hr _ hin).2 d hm rfl⟩
          · exact hp p (List.mem_filter.mp hp').1
  | hangup i =>
      refine ⟨hw, hl, hd, hr, ?_, hc⟩
      intro p hp'
      simp only [step] at hp'
      exact hp p (List.mem_filter.mp hp').1
  | forget ids =>
      refine ⟨hw, hl, ?_, ?_, hp, hc⟩
      · intro d hd'
        simp only [step] at hd'
        exact hd d (List.mem_filter.mp hd').1
      · intro r hr'
        refine ⟨(hr r hr').1, fun d hd' => (hr r hr').2 d ?_⟩
        simp only [step] at hd'
        exact (List.mem_filter.mp hd').1

theorem inv_run (H : Nat → Nat) (evs : List Ev) : ∀ s : St, Inv H s → Inv H (evs.foldl (step impl H) s) := by
  induction evs with
  | nil => intro s h; exact h
  | cons e rest ih => intro s h; exact ih _ (inv_step H s e h)

/-- P5 (and the cookie's P2) from the invariant: a token checks only as a stored device seen within MAX_AGE, minted
with a token of the same hash, never revoked. -/
theorem check_sound (H : Nat → Nat) (s : St) (h : Inv H s) (t : Nat) (d : Dev)
    (hk : check impl H s.now s.devs t = some d) :
    d ∈ s.devs ∧ s.now - d.seen ≤ MAX_AGE ∧ d.serial ∉ s.revoked ∧
      ∃ t', s.minted[d.serial]? = some (d.id, t') ∧ H t' = H t := by
  obtain ⟨hmem, hh, hage⟩ := check_mem H s.now s.devs t d hk
  obtain ⟨_, _, hd, hr, _, _⟩ := h
  refine ⟨hmem, hage, fun hi => (hr _ hi).2 d hmem rfl, ?_⟩
  obtain ⟨t', ht', he⟩ := hd d hmem
  exact ⟨t', ht', he ▸ hh⟩

/-- The code, for every trace and every hash: (P1, P2, P3) every pairing window, the live one and each one a newer
window replaced, let at most one pair through, only before its deadline, and took at most PAIR_TRIES tries, the one
that paired included; (P1, P2) every Mac login code logged in at most once, before its deadline; (P4) a revoked
device is out of the store and has no live peer; (P5, P2) a token checks only as a stored device seen within
MAX_AGE, unrevoked, minted with a token of the same hash; and a success mints at most one device. -/
theorem code_invariant (H : Nat → Nat) (evs : List Ev) :
    (∀ w ∈ (run impl H evs).win.toList ++ (run impl H evs).past,
      w.wins ≤ 1 ∧ (w.wins = 1 → w.wonAt < w.deadline) ∧ w.tries + w.wins ≤ PAIR_TRIES) ∧
    (∀ l ∈ (run impl H evs).logins ++ (run impl H evs).dropped,
      l.uses ≤ 1 ∧ (l.uses = 1 → l.usedAt < l.deadline)) ∧
    (∀ r ∈ (run impl H evs).revoked,
      (∀ d ∈ (run impl H evs).devs, d.serial ≠ r) ∧ ∀ p ∈ (run impl H evs).peers, p.2 ≠ r) ∧
    (∀ t d, check impl H (run impl H evs).now (run impl H evs).devs t = some d →
      d ∈ (run impl H evs).devs ∧ (run impl H evs).now - d.seen ≤ MAX_AGE ∧ d.serial ∉ (run impl H evs).revoked ∧
      ∃ t', (run impl H evs).minted[d.serial]? = some (d.id, t') ∧ H t' = H t) ∧
    (run impl H evs).minted.length ≤ (run impl H evs).pairs + (run impl H evs).macLogins := by
  have hinv : Inv H (run impl H evs) := inv_run H evs {} (inv_init H)
  obtain ⟨hw, hl, hd, hr, hp, hc⟩ := hinv
  refine ⟨?_, ?_, ?_, check_sound H _ ⟨hw, hl, hd, hr, hp, hc⟩, hc⟩
  · intro w hw'
    obtain ⟨h1, h2, h3, _⟩ := hw w hw'
    exact ⟨h2, fun h => (h3 h).2, h1⟩
  · intro l hl'
    obtain ⟨h1, h2, _⟩ := hl l hl'
    exact ⟨h1, fun h => (h2 h).2⟩
  · intro r hr'
    refine ⟨(hr r hr').2, fun p hp' heq => (hp p hp').2 (heq ▸ hr')⟩

/-- P4 for the token itself: when the hash has no collisions and no token was drawn twice, a revoked device's own
token checks as nothing at all. -/
theorem fresh_revoked_token_dies (H : Nat → Nat) (evs : List Ev) (hH : ∀ x y, H x = H y → x = y)
    (hfresh : ∀ (i j : Nat) (a b : Nat × Nat), (run impl H evs).minted[i]? = some a →
      (run impl H evs).minted[j]? = some b →
      a.2 = b.2 → i = j)
    (r rid t : Nat) (hmint : (run impl H evs).minted[r]? = some (rid, t)) (hrev : r ∈ (run impl H evs).revoked) :
    check impl H (run impl H evs).now (run impl H evs).devs t = none := by
  cases hk : check impl H (run impl H evs).now (run impl H evs).devs t with
  | none => rfl
  | some d =>
      obtain ⟨_, _, _, hsound, _⟩ := code_invariant H evs
      obtain ⟨_, _, hnr, t', ht', he⟩ := hsound t d hk
      have htt : t' = t := hH _ _ he
      subst htt
      have := hfresh _ _ _ _ ht' hmint rfl
      exact absurd (this ▸ hrev) hnr

/-! ## Worked traces (each checked by `decide`) -/

/-- The hash of the worked traces: any injective function would do. -/
def h0 (t : Nat) : Nat := t + 1000

/-- A phone scans the QR code: the window pairs it and its cookie checks; the same QR code scanned again, and the
window's 6-digit code, pair nothing more. -/
theorem qr_pairs_once :
    let s := run impl h0 [.openWin 77 123456, .pair (some 77) none [5] 9 true, .pair (some 77) none [6] 10 true,
                          .pair none (some 123456) [7] 11 true]
    s.devs = [⟨5, h0 9, 0, 0⟩] ∧ check impl h0 s.now s.devs 9 = some ⟨5, h0 9, 0, 0⟩ ∧
      check impl h0 s.now s.devs 10 = none ∧ check impl h0 s.now s.devs 11 = none := by
  decide

/-- The secret and the code given together are a wrong try, not a pair. -/
theorem both_given_is_a_try :
    let s := run impl h0 [.openWin 77 123456, .pair (some 77) (some 123456) [5] 9 true]
    s.devs = [] ∧ (s.win.map (·.tries)) = some 1 := by
  decide

/-- The laptop's typed code after five minutes pairs nothing, nor after `close`. -/
theorem expired_or_closed_window_refuses :
    (run impl h0 [.openWin 77 123456, .tick 300, .pair none (some 123456) [5] 9 true]).devs = [] ∧
    (run impl h0 [.openWin 77 123456, .tick 299, .pair none (some 123456) [5] 9 true]).devs = [⟨5, h0 9, 299, 0⟩] ∧
    (run impl h0 [.openWin 77 123456, .closeWin, .pair none (some 123456) [5] 9 true]).devs = [] := by
  decide

/-- Four wrong codes, then the right one, pair (five tries); five wrong codes close the window, and the right one
after them pairs nothing. -/
theorem fifth_wrong_locks :
    (run impl h0 [.openWin 77 123456, .pair none (some 1) [5] 9 true, .pair none (some 2) [5] 9 true,
                  .pair none (some 3) [5] 9 true, .pair none (some 4) [5] 9 true,
                  .pair none (some 123456) [5] 9 true]).devs = [⟨5, h0 9, 0, 0⟩] ∧
    (run impl h0 [.openWin 77 123456, .pair none (some 1) [5] 9 true, .pair none (some 2) [5] 9 true,
                  .pair none (some 3) [5] 9 true, .pair none (some 4) [5] 9 true, .pair none (some 5) [5] 9 true,
                  .pair none (some 123456) [5] 9 true]).devs = [] := by
  decide

/-- "Add a phone" twice: the first window's code is dead, the second's pairs. A pair whose save failed used its
window up all the same. -/
theorem new_window_replaces_old :
    (run impl h0 [.openWin 77 111111, .openWin 88 222222, .pair none (some 111111) [5] 9 true]).devs = [] ∧
    (run impl h0 [.openWin 77 111111, .openWin 88 222222, .pair none (some 222222) [5] 9 true]).devs
      = [⟨5, h0 9, 0, 0⟩] ∧
    (run impl h0 [.openWin 77 111111, .pair none (some 111111) [5] 9 false,
                  .pair none (some 111111) [5] 9 true]).devs = [] := by
  decide

/-- The Mac's login code logs the Mac's browser in once; used again, or after two minutes, it does nothing; and
with sixteen newer codes waiting, the oldest is dropped. -/
theorem mac_login_once :
    (run impl h0 [.issue 42, .consume 42 [5] 9 true, .consume 42 [6] 10 true]).devs = [⟨5, h0 9, 0, 0⟩] ∧
    (run impl h0 [.issue 42, .tick 120, .consume 42 [5] 9 true]).devs = [] ∧
    (run impl h0 [.issue 42, .tick 119, .consume 42 [5] 9 true]).devs = [⟨5, h0 9, 119, 0⟩] ∧
    (run impl h0 ((List.range 17).map (fun i => Ev.issue (100 + i)) ++ [.consume 100 [5] 9 true])).devs = [] ∧
    (run impl h0 ((List.range 17).map (fun i => Ev.issue (100 + i)) ++ [.consume 101 [5] 9 true])).devs
      = [⟨5, h0 9, 0, 0⟩] := by
  decide

/-- Revoking a paired phone: its cookie stops checking and its live peer is closed. A new phone that draws the same
id later is a new device: its own cookie checks, the revoked one's does not. Revoking all logs the Mac out too. -/
theorem revoke_closes :
    (let s := run impl h0 [.openWin 77 123456, .pair (some 77) none [5] 9 true, .connect 9, .revoke (some 5)]
     check impl h0 s.now s.devs 9 = none ∧ s.peers = [] ∧ s.revoked = [0]) ∧
    (let s := run impl h0 [.openWin 77 123456, .pair (some 77) none [5] 9 true, .revoke (some 5),
                           .openWin 78 654321, .pair (some 78) none [5] 10 true]
     check impl h0 s.now s.devs 9 = none ∧ check impl h0 s.now s.devs 10 = some ⟨5, h0 10, 0, 1⟩) ∧
    (let a := run impl h0 [.issue 42, .consume 42 [3] 5 true, .openWin 77 123456, .pair (some 77) none [4] 9 true,
                           .connect 5, .connect 9, .revoke none]
     a.devs = [] ∧ a.peers = [] ∧ a.revoked = [0, 1]) := by
  decide

/-- A cookie whose device was not seen for 400 days no longer checks. -/
theorem cookie_expires :
    (let s := run impl h0 [.openWin 77 123456, .pair (some 77) none [5] 9 true, .tick (MAX_AGE + 1), .request 9]
     check impl h0 s.now s.devs 9 = none) ∧
    (let s := run impl h0 [.openWin 77 123456, .pair (some 77) none [5] 9 true, .tick MAX_AGE, .request 9]
     check impl h0 s.now s.devs 9 = some ⟨5, h0 9, MAX_AGE, 0⟩) := by
  decide

/-- Forgetting is not revoking: a device `_kept` forgot stops checking, and its live peer stays until it hangs up
(the spec closes peers on revoke alone). -/
theorem forget_is_not_revoke :
    let s := run impl h0 [.openWin 77 123456, .pair (some 77) none [5] 9 true, .connect 9, .forget [5]]
    check impl h0 s.now s.devs 9 = none ∧ s.peers = [(5, 0)] ∧ s.revoked = [] := by
  decide

/-! ## Positive controls: each mechanism, taken out alone, breaks its property -/

/-- (P1) Without single use, one QR code pairs two devices, and one login code logs in twice. (P2) Without the
deadlines, a code typed after five minutes pairs, a login code works after two minutes, and a cookie unseen for 400
days still checks. (P3) Without the lockout, a sixth try reaches the window and pairs. (P4) Without closing peers, a
revoked phone keeps its live peer. -/
theorem naive_violates :
    (run { singleUse := false } h0 [.openWin 77 123456, .pair (some 77) none [5] 9 true,
                                    .pair (some 77) none [6] 10 true]).devs = [⟨5, h0 9, 0, 0⟩, ⟨6, h0 10, 0, 1⟩] ∧
    ((run { singleUse := false } h0 [.openWin 77 123456, .pair (some 77) none [5] 9 true,
                                     .pair (some 77) none [6] 10 true]).win.map (·.wins)) = some 2 ∧
    (run { singleUse := false } h0 [.issue 42, .consume 42 [5] 9 true, .consume 42 [6] 10 true]).devs
      = [⟨5, h0 9, 0, 0⟩, ⟨6, h0 10, 0, 1⟩] ∧
    ((run { expires := false } h0 [.openWin 77 123456, .tick 600, .pair none (some 123456) [5] 9 true]).win.map
      (fun w => decide (w.wins = 1 ∧ w.deadline ≤ w.wonAt))) = some true ∧
    ((run { lockout := false } h0 [.openWin 77 123456, .pair none (some 1) [5] 9 true, .pair none (some 2) [5] 9 true,
        .pair none (some 3) [5] 9 true, .pair none (some 4) [5] 9 true, .pair none (some 5) [5] 9 true,
        .pair none (some 123456) [5] 9 true]).win.map (fun w => decide (PAIR_TRIES < w.tries + w.wins))) = some true ∧
    (let s := run { closePeers := false } h0 [.openWin 77 123456, .pair (some 77) none [5] 9 true, .connect 9,
                                              .revoke (some 5)]
     s.revoked = [0] ∧ s.peers = [(5, 0)]) ∧
    (run { expires := false } h0 [.issue 42, .tick 130, .consume 42 [5] 9 true]).devs = [⟨5, h0 9, 130, 0⟩] ∧
    (let s := run { expires := false } h0 [.openWin 77 123456, .pair (some 77) none [5] 9 true, .tick (MAX_AGE + 1)]
     check { expires := false } h0 s.now s.devs 9 = some ⟨5, h0 9, 0, 0⟩) := by
  decide

end Eve.WebPairing
