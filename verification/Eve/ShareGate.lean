/-
  Eve.ShareGate — the consent gate of `share_file`: a file of the owner's goes to another person only on a
  bare Allow of the exact file and recipient, once, and never while another question waits.

  Code modelled (origin/main a35b043; line numbers are of that tree):
  - bridge/src/eve_bridge/share.py
    - `resolve_file` (199-210), which calls telegram.py `resolve_owner_path` (1199-1225): the raw path is
      resolved with every link followed (`Path.resolve`), and must lie in the owner's home with no hidden
      component, exist, and be a regular file. Modelled as `resolveIn`: the world's `resolve` table gives the real
      path, and an arbitrary rule `ok` (every theorem is for EVERY `ok`) says whether it passes;
    - `_open_exact` (281-283), `fingerprint` (286-296): one `O_NOFOLLOW` descriptor, fstat, SHA-256. Modelled as
      `openIn`; `O_NOFOLLOW` refuses a link only in the LAST component, so in `Ver.cur` the open reaches whatever
      object the real path leads to now, including through a folder that became a link (`Obj.loc` is where the
      object really lives);
    - `prepare` (460-494): the rules, the fingerprint, the recipient (`_recipient` 420-457, abstracted as
      `rcpt : Option Nat`), the sweep (`_sweep_pending` 496-499), a fresh token (`secrets.token_urlsafe`, modelled
      as a counter), the `Pending` record (264-274: real path, size, mtime, handle, created, raw, ino, dev, sha256);
    - `cancel` (501-502), `ask_and_share` (362-379): a refusal or a raise of the ask cancels the token; only an
      answer `consent.approves` reads as a yes reaches `confirm`;
    - `confirm` (505-518): pop the token (once), refuse when unknown or `clock() - created > APPROVAL_TTL_SECS`
      (600 s, line 77), resolve the raw path again and require the same real path;
    - `stage_exact` (299-320): open the real path again, compare (ino, dev, size, mtime) and the SHA-256 of the
      bytes copied; on any difference nothing is sent. `_send_imessage` (520-549) and `_send_drive` /
      `_send_drive_staged` (614-648) send that staged copy, never the path opened again (P4). Since 2026-10-09
      (712dd34) a third sink, `_send_gmail` (share.py:604-626), does the same: `stage_exact` (609) into a private
      `TemporaryDirectory`, the mail built from `staged.read_bytes()` (610, `attachment_mail` 723-736), POSTed
      (616-617). At 712dd34 the three senders are `_send_imessage` 563-601 (osascript gets `str(staged)`, 576),
      `_send_gmail` 604-626 and `_send_drive` 682-694 / `_send_drive_staged` 696-716 (stages at 687, uploads
      `staged`); `stage_exact` is 334-355 and `confirm` 546-561, which dispatches on `p.via` and `p.attach`
      (557-561). `sendBytes` abstracts the sink away, so P4 is claimed for Gmail too.
  - bridge/src/eve_bridge/telegram.py
    - `_share_tool` (4908-4923): refuses with `QUESTION_WAITING_REASON` (1140) while `_awaiting_answer`
      (1963-1966) holds, before `prepare`. `_awaiting_answer` reads only the slot `_pending_answer`, which
      `_show_question` (5119-5128) sets on every open and close and, when a future settles, from its done callback
      (5110), which asyncio runs on a later loop turn. Modelled as `St.gateBusy`, with `lag` for "the slot names a
      settled future" (`otherClose stale`, cleared by `sync`, `otherOpen` and the share question's own open);
    - `_ask_alone` (4925-4934): checks `_awaiting_answer` again after `prepare`'s awaits and raises
      `_QuestionWaiting`, which `ask_and_share` turns into `cancel(token)`; otherwise `_ask_user` (5130-5205) with
      `ALLOW_DENY` (872) and `firm=True` opens the question synchronously (`_open_question` 5103-5112), so no
      other question can open between the check and the open;
    - the answers: a tap (`_on_tap` 2733-2760) hands the button's value; a typed or spoken message reaches the
      question only when it is the newest live one (`_show_question` 5119-5128, `_handle` 2484-2494,
      `_hear_answer` 4529-4554), and for a firm question only when `consent.bare_decision` (consent.py 64) reads
      it as a yes or a no (`_answers_pending` 2241-2263), handed on as "yes"/"no" (`_strict_answer` 2265-2273);
      `ask_timeout_secs` gives "no (no answer …)"; a cancelled asker raises (`cancel(token)`).
      Typed text is modelled by what `bare_decision` makes of it (`Option Bool`), for EVERY reading.

  Properties (`Spec`), over every trace of events:
  - (P1) every send is of a token whose question got a bare Allow (a tap, or bare typed words while the share
    question was the newest), that was issued, unexpired at the confirm and not used before, and the staged
    object equals the record: same real path (and that path passes the owner-path rule), inode, device, size,
    mtime and bytes (SHA-256), and the recipient is the recorded one;
  - (P2) each token sends at most once (`once`); a denied, cancelled, timed-out or refused token sends nothing
    (`notRefused`); an expired one sends nothing (`when ≤ created + TTL`);
  - (P3) a share question opens only while no other question is open (`asked0`); a typed answer approves a share
    only while its question is the newest live one (`typed0`); `prepare` runs only while none waits;
  - (P4) the bytes sent are the staged copy's, which are the bytes checked (`bytesStaged`).

  Finding (`current_violates`, replay bridge/tests/test_lean_sharegate_replay.py
  `test_a_folder_swapped_for_a_link_between_resolve_and_open_never_sends_a_hidden_file`): the owner-path rules
  judge the path as resolved at one moment, and the bytes are read by opening that real path again a moment later
  with `O_NOFOLLOW`, which refuses a link only in the last component. A folder on the way that becomes a link in
  between is followed: `~/Documents/inbox/notes.txt` passes the rules, `inbox` becomes a link to `~/.ssh`, and the
  fingerprint, the question ("Send notes.txt … ?"), the Allow, and the staged copy are all of `~/.ssh/notes.txt`.
  At the send the inode, size, mtime and SHA-256 match, because they were taken of the hidden file. P1 fails: the
  object staged does not live at the recorded real path, and its true path breaks the owner-path rule.

  Fix (`Ver.fix`, `fixed_invariant`): every open of the real path (`fingerprint` and `stage_exact`, i.e.
  `_open_exact`) walks it from `/` one component at a time with `O_NOFOLLOW` (`O_DIRECTORY` for the folders,
  `dir_fd=` the previous one), so the object reached is the one that lives at that real path or the open fails
  (`openIn .fix`: `o.loc = r`). A real path from `Path.resolve` has no link in it, so nothing legitimate is
  refused. (Checking the descriptor's own path, `fcntl(F_GETPATH)` on macOS, is equivalent.) With the fix the
  replay passes, and so do the 25 tests of bridge/tests/test_share.py.

  Second finding (`slot_violates`, replay `test_a_share_never_asks_while_an_older_question_still_waits`): the
  gate's `_awaiting_answer` is False while the slot still names a question that just settled, even though an older
  question is still waiting. A share whose `prepare` ends in that tick opens its question over a task's "Reply yes
  or no."; the owner's yes for the task is now the newest question's answer and sends the file (P3: asked with one
  question open, approved by a typed answer while another question was open). Fix (in `Ver.fix` too): both gates
  ask whether ANY question is live, `self._live_question() is not None` (telegram.py 4526), not the slot
  (`gateBusy .fix = awaiting`). Checked in a scratchpad by patching the two methods: the replay passes, and so do
  bridge/tests/test_share.py and test_telegram_toolsets.py.

  Applied on 2026-10-05 (feat/apps-mcp): share.py `_open_exact` walks the real path from its anchor with
  `os.open(part, O_RDONLY|O_NOFOLLOW|O_DIRECTORY, dir_fd=…)` per folder and `O_NOFOLLOW` on the file; telegram.py
  `_share_tool` and `_ask_alone` gate on `self._live_question() is not None`. Both replays pass. The same slot
  read still gates `chief_ask` (not modelled here).

  Positive control (`reopen_violates`): a send that opens the path again after staging, instead of sending the
  staged copy, sends bytes nobody checked (P4).

  Where the Python goes beyond the model:
  - SHA-256 is modelled as the bytes' identity (no collisions); a write during the copy changes the digest and is
    refused, modelled as the object being different (`bytes`) or the same.
  - The staged copy (`~/Pictures/eve-outbox`, 0600, `O_EXCL`; or a private `TemporaryDirectory`) is taken as
    private to eve between the stage and Messages' or Drive's read of it. Another process of the owner's could
    rewrite it; that is outside the model.
  - A hard link: an object at an allowed path that is also a hidden file's inode passes (its `loc` is the allowed
    path). The rules are about paths; hard links are outside them.
  - The size caps (`MAX_IMESSAGE_BYTES`, `MAX_DRIVE_BYTES`, `MAX_GMAIL_ATTACH_BYTES`), the note, `via`, the
    Contacts lookup and its ambiguity, osascript's, Drive's and Gmail's failures only refuse or fail a send; they
    are folded into `rcpt = none` or left out. A send counts as a send once handed to Messages, Drive or Gmail;
    whether it was delivered is outside.
  - The channel (2026-10-09). `Pending.attach` (share.py:287) is computed in `prepare` from the fingerprint's
    size and `Share.gmail` (516), named in the question (529-530) and dispatched on in `confirm` (559): the file
    goes by the channel the question named, by field carry, like `rcpt`. Not modelled (a candidate conjunct of
    `Match`: `e.attach = p.attach ∧ (p.attach = true → p.size ≤ 18 MiB)`); bridge/tests/test_share.py pins it.
  - The clock is `time.monotonic`, so it never steps back (`tick`). The sweep (`_sweep_pending`) is modelled.
  - Tokens are fresh (`secrets.token_urlsafe(12)`, 96 bits): modelled as a counter.
  - Other questions are counted (`others`, and `above`: those opened after the share question), not modelled one
    by one; which one ends is the event's choice (`fromAbove`), only the newest-or-not order is kept; eve/Questions.lean proves the slot names the
    newest live question, which is what routes a typed answer here. An answer typed for a newer question that ends
    just before the answer arrives reaches the share question if it is then the newest; that is the slot's rule,
    not this gate's.
  - `ask_and_share` reads the answer with `consent.approves`, not `bare_decision`; for the firm question every
    answer it can receive is "yes", "no", a button's value, or the timeout's "no (…)", on which the two agree.
    The model takes the answer as the routed yes/no.
-/
namespace Eve.ShareGate

/-- `APPROVAL_TTL_SECS` (share.py:77). -/
def TTL : Nat := 600

/-- A regular file as an open reaches it: `loc` is the link-free real path where it lives (a ghost), then what
`fstat` gives, and its bytes (the SHA-256 is modelled as the bytes' identity). -/
structure Obj where
  loc : Nat
  ino : Nat
  dev : Nat
  size : Nat
  mtime : Nat
  bytes : Nat
  deriving DecidableEq, Repr

/-- The file system at one moment: what `Path.resolve` makes of a raw path, and what an `O_NOFOLLOW` open of a
real path reaches (intermediate links followed). Any other process may change it at any time (`env`, and the
worlds carried by `prepare` and `confirm` for the moments between their awaits). -/
structure World where
  resolve : List (Nat × Nat) := []
  reach : List (Nat × Obj) := []
  deriving DecidableEq, Repr

/-- `Pending` (share.py:260-274). -/
structure Pending where
  tok : Nat
  raw : Nat
  path : Nat
  rcpt : Nat
  ino : Nat
  dev : Nat
  size : Nat
  mtime : Nat
  sha : Nat
  created : Nat
  deriving DecidableEq, Repr

/-- One send: the token, the handle, the object staged, the bytes handed to Messages / Drive, the confirm's time. -/
structure Sent where
  tok : Nat
  rcpt : Nat
  obj : Obj
  bytes : Nat
  when : Nat
  deriving DecidableEq, Repr

/-- `cur`: a35b043. `fix`: every open walks the real path with `O_NOFOLLOW`, and the share gates ask
`_live_question()` instead of the slot. `reopen`: the fix, but the send opens
the path again instead of sending the staged copy (the positive control for P4). -/
inductive Ver where
  | cur
  | fix
  | reopen
  deriving DecidableEq, Repr

inductive Event where
  /-- the monotonic clock moves on -/
  | tick (dt : Nat)
  /-- the file system changes -/
  | env (w : World)
  /-- `share_file` called: `_share_tool`'s check, then `prepare`; `w` is the world when `fingerprint` opens -/
  | prepare (raw : Nat) (rcpt : Option Nat) (w : World)
  /-- `_ask_alone` for the token `prepare` gave -/
  | ask (tok : Nat)
  /-- another question opens (a task's, a permission prompt, an app consent, the chief's Go, another share);
  `_open_question` runs `_show_question` at once, so the slot names it -/
  | otherOpen
  /-- another question ends. `fromAbove`: it was opened after the share question open now (else before it).
  `stale`: its future settled while the slot named it, and the done callback `_show_question` has not run yet,
  so the slot still names a done future (telegram.py 5110: `add_done_callback`, run on a later loop turn) -/
  | otherClose (stale fromAbove : Bool)
  /-- the slot's done callback runs: `_show_question` names the newest live question again -/
  | sync
  /-- a tap on the share question's Allow (true) or Deny (false) -/
  | tap (yes : Bool)
  /-- a typed or spoken message; `bare` is what `consent.bare_decision` makes of it -/
  | typed (bare : Option Bool)
  /-- `ask_timeout_secs` passed: "no (no answer …)" -/
  | timeout
  /-- the asker went away (CancelledError): `cancel(token)` -/
  | gone
  /-- `confirm(token)` after the Allow; `w` is the world when `stage_exact` opens, `wSend` when the send runs -/
  | confirm (tok : Nat) (w wSend : World)
  deriving Repr

structure St where
  now : Nat := 0
  world : World := {}
  next : Nat := 0                    -- the next fresh token
  store : List Pending := []         -- `Share._pending`
  others : Nat := 0                  -- other open questions
  above : Nat := 0                   -- of those, the ones opened after the share question open now
  lag : Bool := false                -- the slot names a settled future (its done callback not yet run)
  shareQ : Option Nat := none        -- the share question open, by its token
  approved : List Nat := []          -- answered Allow, `confirm` not yet run
  issued : List Pending := []        -- ghost: every Pending ever made
  allowed : List Nat := []           -- ghost: tokens whose question got a bare Allow
  refused : List Nat := []           -- ghost: tokens denied, timed out, cancelled or refused while pending
  askedWith : List Nat := []         -- ghost: the open-question count at each share question's open
  typedWith : List Nat := []         -- ghost: other questions open at each typed answer to a share question
  sent : List Sent := []
  deriving Repr

/-- `_awaiting_answer` (telegram.py:1963-1966): some question's future is not done. -/
def St.awaiting (s : St) : Bool := s.others > 0 || s.shareQ.isSome

def St.openCount (s : St) : Nat := s.others + (if s.shareQ.isSome then 1 else 0)

/-- The gate of `_share_tool` (4916) and `_ask_alone` (4931). At a35b043 it is `_awaiting_answer`, which reads only
the slot: while the slot names a settled future (`lag`) it is False even though older questions still wait. The fix
asks whether ANY question is live (`self._live_question() is not None`, telegram.py 4526), i.e. `awaiting`. -/
def St.gateBusy (v : Ver) (s : St) : Bool :=
  match v with
  | .cur => !s.lag && s.awaiting
  | _ => s.awaiting

/-- `resolve_file` → `resolve_owner_path`: the real path, if it passes the owner-path rule `ok`. -/
def resolveIn (ok : Nat → Bool) (w : World) (raw : Nat) : Option Nat :=
  match w.resolve.lookup raw with
  | some r => if ok r then some r else none
  | none => none

/-- `_open_exact`: at a35b043 the last component only is `O_NOFOLLOW`; the fix walks every component. -/
def openIn : Ver → World → Nat → Option Obj
  | .cur, w, r => w.reach.lookup r
  | _, w, r => match w.reach.lookup r with
      | some o => if o.loc = r then some o else none
      | none => none

def findTok (t : Nat) (l : List Pending) : Option Pending := l.find? (fun p => p.tok == t)
def dropTok (t : Nat) (l : List Pending) : List Pending := l.filter (fun p => p.tok != t)
/-- `_sweep_pending` (share.py:496-499): keep `now - created ≤ TTL`. -/
def sweep (now : Nat) (l : List Pending) : List Pending := l.filter (fun p => decide (now ≤ p.created + TTL))

/-- `cancel(token)` (share.py:501-502); the ghost records a token it really dropped. -/
def St.cancelTok (s : St) (t : Nat) : St :=
  if s.store.any (fun p => p.tok == t) then
    { s with store := dropTok t s.store, refused := t :: s.refused }
  else s

/-- The share question's future settles: an Allow goes on to `confirm`, anything else `cancel`s. -/
def St.answer (s : St) (t : Nat) (yes : Bool) : St :=
  if yes then { s with shareQ := none, approved := t :: s.approved, allowed := t :: s.allowed }
  else ({ s with shareQ := none } : St).cancelTok t

/-- What is handed to Messages / Gmail / Drive: the staged copy (share.py at 712dd34: `_send_imessage` 576
`str(staged)`, `_send_gmail` 610 `staged.read_bytes()`, `_send_drive` 694 → `_send_drive_staged` uploads `staged`;
at a35b043, 533 and 626), or in `reopen` the path again. -/
def sendBytes (v : Ver) (wSend : World) (path : Nat) (staged : Nat) : Nat :=
  match v with
  | .reopen => match wSend.reach.lookup path with
      | some o => o.bytes
      | none => staged
  | _ => staged

/-- `confirm` pops the token whatever happens next (share.py:508). -/
def St.dropped (s : St) (t : Nat) : St := { s with approved := s.approved.erase t, store := dropTok t s.store }

/-- `confirm` (share.py:505-518) then `stage_exact` (299-320) and the send. -/
def St.confirm (v : Ver) (ok : Nat → Bool) (s : St) (t : Nat) (w wSend : World) : St :=
  if !(s.approved.contains t) then s else
  match findTok t s.store with
  | none => s.dropped t
  | some p =>
    if p.created + TTL < s.now then s.dropped t else
    match resolveIn ok s.world p.raw with
    | none => s.dropped t
    | some r =>
      if r ≠ p.path then s.dropped t else
      match openIn v w p.path with
      | none => { s.dropped t with world := w }
      | some o =>
        if o.ino = p.ino ∧ o.dev = p.dev ∧ o.size = p.size ∧ o.mtime = p.mtime ∧ o.bytes = p.sha then
          { s.dropped t with world := wSend,
                             sent := ⟨t, p.rcpt, o, sendBytes v wSend p.path o.bytes, s.now⟩ :: s.sent }
        else { s.dropped t with world := w }

def step (v : Ver) (ok : Nat → Bool) (s : St) : Event → St
  | .tick dt => { s with now := s.now + dt }
  | .env w => { s with world := w }
  -- _share_tool 4916-4917, then prepare 460-494 (resolve_file 468, fingerprint 474, _recipient 481, 485-488)
  | .prepare raw rcpt w =>
      if s.gateBusy v then s else
      match resolveIn ok s.world raw with
      | none => s
      | some r =>
        match openIn v w r, rcpt with
        | some o, some c =>
          let p : Pending := ⟨s.next, raw, r, c, o.ino, o.dev, o.size, o.mtime, o.bytes, s.now⟩
          { s with world := w, next := s.next + 1, store := p :: sweep s.now s.store,
                   issued := p :: s.issued }
        | _, _ => { s with world := w }
  -- _ask_alone 4931-4934; a refusal raises _QuestionWaiting and ask_and_share 373-375 cancels
  | .ask t => if s.gateBusy v then s.cancelTok t
              else { s with shareQ := some t, askedWith := s.openCount :: s.askedWith, above := 0, lag := false }
  | .otherOpen => { s with others := s.others + 1, above := s.above + 1, lag := false }
  | .otherClose stale fromAbove =>
      if fromAbove then { s with others := s.others - 1, above := s.above - 1, lag := stale }
      else if s.above < s.others then { s with others := s.others - 1, lag := stale } else s
  | .sync => { s with lag := false }
  -- _on_tap 2750-2758: the button's own question, newest or not
  | .tap b => match s.shareQ with
      | some t => s.answer t b
      | none => s
  -- _handle 2484-2494 with _answers_pending / _strict_answer: only to the newest live question, only bare words;
  -- while the slot is stale `_awaiting_answer` is False and no text is an answer
  | .typed bare => match s.shareQ, bare with
      | some t, some b => if s.above = 0 ∧ s.lag = false then
            { s.answer t b with typedWith := s.others :: s.typedWith } else s
      | _, _ => s
  | .timeout => match s.shareQ with
      | some t => s.answer t false
      | none => s
  | .gone => match s.shareQ with
      | some t => ({ s with shareQ := none } : St).cancelTok t
      | none => s
  | .confirm t w wSend => s.confirm v ok t w wSend

def run (v : Ver) (ok : Nat → Bool) (evs : List Event) : St := evs.foldl (step v ok) {}

/-- A send and the record of its token agree (P1's equalities). -/
def Match (ok : Nat → Bool) (e : Sent) (p : Pending) : Prop :=
  p.tok = e.tok ∧ e.rcpt = p.rcpt ∧ e.obj.loc = p.path ∧ ok p.path = true ∧ e.obj.ino = p.ino ∧
    e.obj.dev = p.dev ∧ e.obj.size = p.size ∧ e.obj.mtime = p.mtime ∧ e.obj.bytes = p.sha ∧
    e.when ≤ p.created + TTL

instance (ok : Nat → Bool) (e : Sent) (p : Pending) : Decidable (Match ok e p) := by
  unfold Match; infer_instance

/-- P1-P4. -/
def Spec (ok : Nat → Bool) (s : St) : Prop :=
  (∀ e ∈ s.sent, e.tok ∈ s.allowed ∧ ∃ p ∈ s.issued, Match ok e p) ∧   -- P1
  (s.issued.map Pending.tok).Nodup ∧                                   -- P1: one record per token
  (s.sent.map Sent.tok).Nodup ∧                                        -- P2: once
  (∀ e ∈ s.sent, e.tok ∉ s.refused) ∧                                  -- P2: denied/cancelled send nothing
  (∀ n ∈ s.askedWith, n = 0) ∧ (∀ n ∈ s.typedWith, n = 0) ∧            -- P3
  (∀ e ∈ s.sent, e.bytes = e.obj.bytes)                                -- P4

instance (ok : Nat → Bool) (s : St) : Decidable (Spec ok s) := by
  unfold Spec; infer_instance

/-! ## The finding -/

/-- Real paths under 100 pass the owner-path rule; 110 is `~/.ssh/notes.txt`. -/
def okHome (p : Nat) : Bool := decide (p < 100)

/-- `~/Documents/inbox/notes.txt` (real path 10), and `~/.ssh/notes.txt` (110). -/
def objA : Obj := ⟨10, 1, 1, 14, 5, 111⟩
def objS : Obj := ⟨110, 2, 1, 47, 3, 666⟩
/-- `inbox` is a folder: raw path 1 resolves to 10. -/
def w0 : World := { resolve := [(1, 10)], reach := [(10, objA), (110, objS)] }
/-- `inbox` is a link to `~/.ssh`: path 10 now reaches the hidden file. -/
def w1 : World := { resolve := [(1, 110)], reach := [(10, objS), (110, objS)] }

/-- The replay's trace: the rules pass on the folder, the fingerprint opens through the link, the owner taps
Allow, the folder is back for `confirm`'s resolve and a link again for `stage_exact`'s open. -/
def swapTrace : List Event :=
  [.env w0, .prepare 1 (some 7) w1, .ask 0, .tap true, .env w0, .confirm 0 w1 w1]

theorem current_violates :
    let s := run .cur okHome swapTrace
    s.sent.map (fun e => (e.obj.loc, e.bytes)) = [(110, 666)] ∧ s.issued.map Pending.path = [10] ∧
      ¬ Spec okHome s := by
  decide

/-- The same trace with the fix: the fingerprint's open fails, nothing is asked about, nothing is sent. -/
theorem fixed_replays_counterexample :
    (run .fix okHome swapTrace).sent = [] ∧ (run .fix okHome swapTrace).issued = [] := by
  decide

/-- P4's positive control: sending the path opened again sends bytes nobody checked. -/
def w2 : World := { resolve := [(1, 10)], reach := [(10, { objA with bytes := 999 })] }

theorem reopen_violates :
    let s := run .reopen okHome [.env w0, .prepare 1 (some 7) w0, .ask 0, .tap true, .confirm 0 w0 w2]
    s.sent.map Sent.bytes = [999] ∧ ¬ Spec okHome s := by
  decide

/-- The second finding's trace (replay `test_a_share_never_asks_while_an_older_question_still_waits`): no question
waits when `share_file` starts; while it looks the recipient up a task asks "Reply yes or no." and then a prompt
opens above it; the owner answers the prompt in the tick the lookup returns, so `_ask_alone` reads a stale slot
(`_awaiting_answer` False) and opens the share question over the task's; the owner's yes, for the task, is the
newest question's answer and the file goes. -/
def slotTrace : List Event :=
  [.env w0, .prepare 1 (some 7) w0, .otherOpen, .otherOpen, .otherClose true true, .ask 0, .typed (some true),
   .confirm 0 w0 w0]

theorem slot_violates :
    let s := run .cur okHome slotTrace
    s.sent.map Sent.tok = [0] ∧ s.askedWith = [1] ∧ s.typedWith = [1] ∧ ¬ Spec okHome s := by
  decide

/-- The same trace with the fix (the gates ask `_live_question()`): the share question is refused and cancelled. -/
theorem fixed_replays_slot :
    (run .fix okHome slotTrace).sent = [] ∧ (run .fix okHome slotTrace).askedWith = [] := by
  decide

/-! ## The fixed code, every trace -/

structure Inv (ok : Nat → Bool) (s : St) : Prop where
  sentOk : ∀ e ∈ s.sent, e.tok ∈ s.allowed ∧ ∃ p ∈ s.issued, Match ok e p
  issuedNodup : (s.issued.map Pending.tok).Nodup
  once : (s.sent.map Sent.tok).Nodup
  notRefused : ∀ e ∈ s.sent, e.tok ∉ s.refused
  asked0 : ∀ n ∈ s.askedWith, n = 0
  typed0 : ∀ n ∈ s.typedWith, n = 0
  bytesStaged : ∀ e ∈ s.sent, e.bytes = e.obj.bytes
  storeIssued : ∀ p ∈ s.store, p ∈ s.issued
  issuedLt : ∀ p ∈ s.issued, p.tok < s.next
  refusedLt : ∀ t ∈ s.refused, t < s.next
  sentStore : ∀ e ∈ s.sent, ∀ p ∈ s.store, p.tok ≠ e.tok
  refusedStore : ∀ t ∈ s.refused, ∀ p ∈ s.store, p.tok ≠ t
  approvedAllowed : ∀ t ∈ s.approved, t ∈ s.allowed
  aboveAll : s.shareQ.isSome → s.above = s.others

theorem inv_init (ok : Nat → Bool) : Inv ok {} := by
  constructor <;> simp

theorem Inv.spec {ok : Nat → Bool} {s : St} (h : Inv ok s) : Spec ok s :=
  ⟨h.sentOk, h.issuedNodup, h.once, h.notRefused, h.asked0, h.typed0, h.bytesStaged⟩

theorem mem_dropTok {t : Nat} {l : List Pending} {p : Pending} (h : p ∈ dropTok t l) : p ∈ l ∧ p.tok ≠ t := by
  simp only [dropTok, List.mem_filter, bne_iff_ne, ne_eq] at h
  exact ⟨h.1, h.2⟩

theorem mem_sweep {n : Nat} {l : List Pending} {p : Pending} (h : p ∈ sweep n l) : p ∈ l := by
  simp only [sweep, List.mem_filter] at h
  exact h.1

/-- A step that leaves the sends, the records and the token counter alone, only drops pending tokens and adds
approvals that are allowed, keeps the invariant. -/
theorem inv_frame {ok : Nat → Bool} {s s' : St} (h : Inv ok s)
    (hsent : s'.sent = s.sent) (hiss : s'.issued = s.issued) (hnext : s'.next = s.next)
    (hstore : ∀ p ∈ s'.store, p ∈ s.store) (hall : ∀ t ∈ s.allowed, t ∈ s'.allowed)
    (href : s'.refused = s.refused) (happ : ∀ t ∈ s'.approved, t ∈ s'.allowed)
    (hask : ∀ n ∈ s'.askedWith, n = 0) (htyp : ∀ n ∈ s'.typedWith, n = 0)
    (habove : s'.shareQ.isSome → s'.above = s'.others) : Inv ok s' where
  sentOk := by
    intro e he; rw [hsent] at he
    obtain ⟨ha, p, hp, hm⟩ := h.sentOk e he
    exact ⟨hall _ ha, p, hiss ▸ hp, hm⟩
  issuedNodup := hiss ▸ h.issuedNodup
  once := hsent ▸ h.once
  notRefused := by rw [hsent, href]; exact h.notRefused
  asked0 := hask
  typed0 := htyp
  bytesStaged := hsent ▸ h.bytesStaged
  storeIssued := fun p hp => hiss ▸ h.storeIssued p (hstore p hp)
  issuedLt := by rw [hiss, hnext]; exact h.issuedLt
  refusedLt := by rw [href, hnext]; exact h.refusedLt
  sentStore := fun e he p hp => h.sentStore e (hsent ▸ he) p (hstore p hp)
  refusedStore := fun t ht p hp => h.refusedStore t (href ▸ ht) p (hstore p hp)
  approvedAllowed := happ
  aboveAll := habove

theorem inv_cancel {ok : Nat → Bool} {s : St} (h : Inv ok s) (t : Nat) : Inv ok (s.cancelTok t) := by
  unfold St.cancelTok
  split
  · rename_i hany
    obtain ⟨q, hq, hqt⟩ := List.any_eq_true.mp hany
    have hqt' : q.tok = t := by simpa using hqt
    exact {
      sentOk := h.sentOk
      issuedNodup := h.issuedNodup
      once := h.once
      notRefused := by
        intro e he hr
        rcases List.mem_cons.mp hr with hr | hr
        · exact h.sentStore e he q hq (hqt'.trans hr.symm)
        · exact h.notRefused e he hr
      asked0 := h.asked0
      typed0 := h.typed0
      bytesStaged := h.bytesStaged
      storeIssued := fun p hp => h.storeIssued p (mem_dropTok hp).1
      issuedLt := h.issuedLt
      refusedLt := by
        intro u hu
        rcases List.mem_cons.mp hu with hu | hu
        · subst hu; rw [← hqt']; exact h.issuedLt q (h.storeIssued q hq)
        · exact h.refusedLt u hu
      sentStore := fun e he p hp => h.sentStore e he p (mem_dropTok hp).1
      refusedStore := by
        intro u hu p hp
        rcases List.mem_cons.mp hu with hu | hu
        · subst hu; exact (mem_dropTok hp).2
        · exact h.refusedStore u hu p (mem_dropTok hp).1
      approvedAllowed := h.approvedAllowed
      aboveAll := h.aboveAll }
  · exact h

theorem inv_answer {ok : Nat → Bool} {s : St} (h : Inv ok s) (t : Nat) (b : Bool) : Inv ok (s.answer t b) := by
  unfold St.answer
  cases b
  · simp only [Bool.false_eq_true, ↓reduceIte]
    exact inv_cancel (inv_frame (s' := { s with shareQ := none }) h rfl rfl rfl (fun _ hp => hp)
      (fun _ ht => ht) rfl h.approvedAllowed h.asked0 h.typed0 (fun hq => by simp at hq)) t
  · simp only [↓reduceIte]
    refine inv_frame h rfl rfl rfl (fun _ hp => hp) (fun _ ht => List.mem_cons_of_mem _ ht) rfl ?_ h.asked0 h.typed0
      (fun hq => by simp at hq)
    intro u hu
    rcases List.mem_cons.mp hu with hu | hu
    · subst hu; exact List.mem_cons_self
    · exact List.mem_cons_of_mem _ (h.approvedAllowed u hu)

theorem resolveIn_ok {ok : Nat → Bool} {w : World} {raw r : Nat} (h : resolveIn ok w raw = some r) :
    ok r = true := by
  unfold resolveIn at h
  split at h
  · split at h
    · simp at h; subst h; assumption
    · simp at h
  · simp at h

theorem openIn_fix {w : World} {r : Nat} {o : Obj} (h : openIn .fix w r = some o) : o.loc = r := by
  simp only [openIn] at h
  split at h
  · split at h
    · simp at h; subst h; assumption
    · simp at h
  · simp at h

theorem inv_prepare {ok : Nat → Bool} {s : St} (h : Inv ok s) (raw : Nat) (rcpt : Option Nat) (w : World) :
    Inv ok (step .fix ok s (.prepare raw rcpt w)) := by
  simp only [step]
  split
  · exact h
  · split
    · exact h
    · rename_i r _
      split
      · rename_i o c _ _
        -- the new record has the fresh token `s.next`
        have hfresh : ∀ p ∈ s.issued, p.tok ≠ s.next := fun p hp => Nat.ne_of_lt (h.issuedLt p hp)
        exact {
          sentOk := by
            intro e he
            obtain ⟨ha, p, hp, hm⟩ := h.sentOk e he
            exact ⟨ha, p, List.mem_cons_of_mem _ hp, hm⟩
          issuedNodup := by
            simp only [List.map_cons, List.nodup_cons, List.mem_map, not_exists, not_and]
            exact ⟨fun p hp heq => hfresh p hp heq, h.issuedNodup⟩
          once := h.once
          notRefused := h.notRefused
          asked0 := h.asked0
          typed0 := h.typed0
          bytesStaged := h.bytesStaged
          storeIssued := by
            intro p hp
            rcases List.mem_cons.mp hp with hp | hp
            · subst hp; exact List.mem_cons_self
            · exact List.mem_cons_of_mem _ (h.storeIssued p (mem_sweep hp))
          issuedLt := by
            intro p hp
            rcases List.mem_cons.mp hp with hp | hp
            · subst hp; exact Nat.lt_succ_self _
            · exact Nat.lt_succ_of_lt (h.issuedLt p hp)
          refusedLt := fun t ht => Nat.lt_succ_of_lt (h.refusedLt t ht)
          sentStore := by
            intro e he p hp
            rcases List.mem_cons.mp hp with hp | hp
            · subst hp
              obtain ⟨_, q, hq, hm⟩ := h.sentOk e he
              intro heq
              exact hfresh q hq (hm.1.trans heq.symm)
            · exact h.sentStore e he p (mem_sweep hp)
          refusedStore := by
            intro t ht p hp
            rcases List.mem_cons.mp hp with hp | hp
            · subst hp; exact fun heq => Nat.ne_of_lt (h.refusedLt t ht) heq.symm
            · exact h.refusedStore t ht p (mem_sweep hp)
          approvedAllowed := h.approvedAllowed
          aboveAll := h.aboveAll }
      · exact inv_frame (s' := { s with world := w }) h rfl rfl rfl (fun _ hp => hp) (fun _ ht => ht) rfl
          h.approvedAllowed h.asked0 h.typed0 h.aboveAll

theorem inv_confirm {ok : Nat → Bool} {s : St} (h : Inv ok s) (t : Nat) (w wSend : World) :
    Inv ok (s.confirm .fix ok t w wSend) := by
  unfold St.confirm
  split
  · exact h
  · rename_i hc
    have hct : t ∈ s.approved := by simpa using hc
    -- the frame for every branch that sends nothing
    have hdrop : ∀ (wd : World), Inv ok { s.dropped t with world := wd } := fun wd =>
      inv_frame h rfl rfl rfl (fun p hp => (mem_dropTok hp).1) (fun _ ht => ht) rfl
        (fun u hu => h.approvedAllowed u (List.mem_of_mem_erase hu)) h.asked0 h.typed0 h.aboveAll
    have hdrop0 : Inv ok (s.dropped t) :=
      inv_frame h rfl rfl rfl (fun p hp => (mem_dropTok hp).1) (fun _ ht => ht) rfl
        (fun u hu => h.approvedAllowed u (List.mem_of_mem_erase hu)) h.asked0 h.typed0 h.aboveAll
    split
    · exact hdrop0
    · rename_i p hfind
      have hpmem : p ∈ s.store := List.mem_of_find?_eq_some hfind
      have hpt : p.tok = t := by simpa using List.find?_some hfind
      split
      · exact hdrop0
      · rename_i hexp
        split
        · exact hdrop0
        · rename_i r hr
          split
          · exact hdrop0
          · rename_i hrp
            have hrp' : r = p.path := Classical.byContradiction hrp
            split
            · exact hdrop w
            · rename_i o ho
              split
              · rename_i hf
                obtain ⟨hino, hdev, hsize, hmtime, hbytes⟩ := hf
                have hloc : o.loc = p.path := (openIn_fix ho)
                have hok : ok p.path = true := hrp' ▸ resolveIn_ok hr
                have hpi : p ∈ s.issued := h.storeIssued p hpmem
                exact {
                  sentOk := by
                    intro e he
                    rcases List.mem_cons.mp he with he | he
                    · subst he
                      refine ⟨h.approvedAllowed t hct, p, hpi, hpt, rfl, hloc, hok, hino, hdev, hsize, hmtime,
                        hbytes, ?_⟩
                      exact Nat.le_of_not_lt hexp
                    · exact h.sentOk e he
                  issuedNodup := h.issuedNodup
                  once := by
                    simp only [List.map_cons, List.nodup_cons, List.mem_map, not_exists, not_and]
                    refine ⟨fun e he heq => h.sentStore e he p hpmem (hpt.trans heq.symm), h.once⟩
                  notRefused := by
                    intro e he
                    rcases List.mem_cons.mp he with he | he
                    · subst he; exact fun hr' => h.refusedStore t hr' p hpmem hpt
                    · exact h.notRefused e he
                  asked0 := h.asked0
                  typed0 := h.typed0
                  bytesStaged := by
                    intro e he
                    rcases List.mem_cons.mp he with he | he
                    · subst he; rfl
                    · exact h.bytesStaged e he
                  storeIssued := fun q hq => h.storeIssued q (mem_dropTok hq).1
                  issuedLt := h.issuedLt
                  refusedLt := h.refusedLt
                  sentStore := by
                    intro e he q hq
                    rcases List.mem_cons.mp he with he | he
                    · subst he; exact (mem_dropTok hq).2
                    · exact h.sentStore e he q (mem_dropTok hq).1
                  refusedStore := fun u hu q hq => h.refusedStore u hu q (mem_dropTok hq).1
                  approvedAllowed := fun u hu => h.approvedAllowed u (List.mem_of_mem_erase hu)
                  aboveAll := h.aboveAll }
              · exact hdrop w

theorem inv_step {ok : Nat → Bool} {s : St} (h : Inv ok s) (e : Event) : Inv ok (step .fix ok s e) := by
  have fr : ∀ {s' : St}, s'.sent = s.sent → s'.issued = s.issued → s'.next = s.next → s'.store = s.store →
      s'.allowed = s.allowed → s'.refused = s.refused → s'.approved = s.approved → s'.askedWith = s.askedWith →
      s'.typedWith = s.typedWith → (s'.shareQ.isSome → s'.above = s'.others) → Inv ok s' :=
    fun h1 h2 h3 h4 h5 h6 h7 h8 h9 h10 =>
      inv_frame h h1 h2 h3 (fun p hp => h4 ▸ hp) (fun t ht => h5 ▸ ht) h6 (fun t ht => h5 ▸ h.approvedAllowed t (h7 ▸ ht))
        (fun n hn => h.asked0 n (h8 ▸ hn)) (fun n hn => h.typed0 n (h9 ▸ hn)) h10
  cases e with
  | tick dt => exact fr rfl rfl rfl rfl rfl rfl rfl rfl rfl h.aboveAll
  | env w => exact fr rfl rfl rfl rfl rfl rfl rfl rfl rfl h.aboveAll
  | prepare raw rcpt w => exact inv_prepare h raw rcpt w
  | ask t =>
      simp only [step]
      split
      · exact inv_cancel h t
      · rename_i hw
        simp only [St.gateBusy, St.awaiting, Bool.or_eq_true, decide_eq_true_eq, Option.isSome_iff_exists,
          not_or, not_exists] at hw
        refine inv_frame h rfl rfl rfl (fun _ hp => hp) (fun _ ht => ht) rfl h.approvedAllowed ?_ h.typed0
          (fun _ => by show (0 : Nat) = s.others; omega)
        intro n hn
        rcases List.mem_cons.mp hn with hn | hn
        · subst hn
          cases hq : s.shareQ with
          | none => simp [St.openCount, hq]; omega
          | some q => exact absurd hq (hw.2 q)
        · exact h.asked0 n hn
  | otherOpen =>
      exact fr rfl rfl rfl rfl rfl rfl rfl rfl rfl (fun hq => by
        have := h.aboveAll hq; show s.above + 1 = s.others + 1; omega)
  | otherClose stale fromAbove =>
      simp only [step]
      split
      · exact fr rfl rfl rfl rfl rfl rfl rfl rfl rfl (fun hq => by
          have := h.aboveAll hq; show s.above - 1 = s.others - 1; omega)
      · split
        · rename_i hlt
          exact fr rfl rfl rfl rfl rfl rfl rfl rfl rfl (fun hq => by
            have := h.aboveAll hq; show s.above = s.others - 1; omega)
        · exact h
  | sync => exact fr rfl rfl rfl rfl rfl rfl rfl rfl rfl h.aboveAll
  | tap b =>
      simp only [step]
      split
      · exact inv_answer h _ b
      · exact h
  | typed bare =>
      cases hq : s.shareQ with
      | none => simp only [step, hq]; exact h
      | some t =>
        cases bare with
        | none => simp only [step, hq]; exact h
        | some b =>
          simp only [step, hq]
          split
          · rename_i h0
            have ha := inv_answer h t b
            have hab : s.above = s.others := h.aboveAll (by simp [hq])
            refine inv_frame ha rfl rfl rfl (fun _ hp => hp) (fun _ ht => ht) rfl ha.approvedAllowed ha.asked0 ?_
              ha.aboveAll
            intro n hn
            rcases List.mem_cons.mp hn with hn | hn
            · rw [hn, ← hab]; exact h0.1
            · exact h.typed0 n hn
          · exact h
  | timeout =>
      simp only [step]
      split
      · exact inv_answer h _ false
      · exact h
  | gone =>
      simp only [step]
      split
      · exact inv_cancel (inv_frame (s' := { s with shareQ := none }) h rfl rfl rfl (fun _ hp => hp)
          (fun _ ht => ht) rfl h.approvedAllowed h.asked0 h.typed0 (fun hq => by simp at hq)) _
      · exact h
  | confirm t w wSend => exact inv_confirm h t w wSend

theorem inv_run (ok : Nat → Bool) (evs : List Event) : ∀ s : St, Inv ok s → Inv ok (evs.foldl (step .fix ok) s) := by
  induction evs with
  | nil => intro s h; exact h
  | cons e rest ih => intro s h; exact ih _ (inv_step h e)

/-- P1-P4 hold for the fixed code, for every owner-path rule `ok` and every trace of events. -/
theorem fixed_invariant (ok : Nat → Bool) (evs : List Event) : Spec ok (run .fix ok evs) :=
  (inv_run ok evs {} (inv_init ok)).spec

end Eve.ShareGate
