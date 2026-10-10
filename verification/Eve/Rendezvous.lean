/-
  Eve.Rendezvous — Eve Link's rendezvous: the daemon posts its quick tunnel's address to an ntfy.sh topic that
  only the link token names, so the phone finds eve's new address by itself.

  Models:
  - bridge/src/eve_bridge/miniapp.py: `MiniApp._announce_loop` (lines 1058-1075), and the two writes of
    `self.url` in `MiniApp.run`: `self.url = await tunnel.start(port)` (1090) and `self.url = ""` (1100);
  - bridge/src/eve_bridge/stick_link.py: `publish_address` (123-137; True on a 200, False on a non-200 or an
    `OSError`, and it RAISES any other exception: its `except` catches `OSError` only, lines 130-132),
    `REPOST_SECS = 6 * 3600.0` (96), `LinkToken.current` (54-59; "" when the file cannot be read) and
    `LinkToken.rotate` (72-76; `ensure` is `current() or rotate()`);
  - ntfy.sh, which keeps a message 12 hours, and the phone's reader, ios/EveLink/Shared/Rendezvous.swift
    `newest(in:)` (20-31), which believes the last message the topic still holds;
  - the task's life: `MiniApp.run` starts the loop as a fire-and-forget task (`asyncio.create_task`, 1085) and
    only cancels it in its `finally` (1104); nothing awaits it, so an exception out of the loop ends it silently.
  Line numbers above are those of commit 2ad3550, the code the findings were proved on (`Cur`).
  The fixed code (`Fix`, branch fix/lean-findings, 2026-10-04): miniapp.py `_announce_loop` (1071-1094) reads
  `url, now = self.url, clock()` before the post (1087), posts `url` (1089) and records `sent, sent_at = (url,
  token), now` (1090); the whole iteration is in a `try` whose `except Exception` (1091-1092) logs the type name
  and goes on to the sleep (1093). stick_link.py `publish_address` (123-140) catches `Exception`, not just
  `OSError` (130-134), and returns False. `self.url` is written at miniapp.py:1109 and 1119; the task is made at
  1104 and cancelled in `run`'s `finally`.

  Time is a natural number of poll ticks: one tick is `every` (30 s). REPOST_SECS is 720 ticks and ntfy's cache
  1440 ticks. A post (`publish_address`, a 10 s timeout per socket operation) is shorter than a tick, so a poll and
  its post happen inside one tick; the tunnel's address and the token may still change while the post is in flight
  (the events between `wake` and `ok`/`fail`/`raises`). The model does not advance time while a post is in
  flight, so its ticks count polls: "tick 721" below is the 720th poll after the post of A. By the real clock
  (`clock()`, which the loop reads) the repost comes REPOST_SECS = 6 h after that post, which is at most 720 polls
  when posts take time.

  The properties, over every trace of events:
  - (P1) freshness. At every moment the loop sleeps, if neither the address nor the token changed since that poll
    began, the poll's post (if it made one) succeeded and the token was read, and there is an address and a token,
    then the newest message on the token's topic is the current address, and ntfy still holds it. So after a
    change, the topic is right once the first poll that begins after the change has posted: a stale window is at
    most one `every` plus one post, given success.
  - (P2) no spam. Two successful posts of the same (address, token) are at least REPOST_SECS apart.
  - (P3) a failed post is retried on the very next poll, not after REPOST_SECS: when the loop sleeps after a
    failed post and nothing changed since, the next poll posts.
  - (P4) the loop lives: whatever a post does, the loop comes back to its sleep and polls again (`Alive`).

  Finding 1 (the code at 2ad3550, P1; FIXED). `_announce_loop` reads `self.url` twice around the await: the post carries the
  address of the call (miniapp.py:1073, `publish_address, self.url, token, base`), but the bookkeeping records the
  address of the moment the post returns (1074, `sent, sent_at = (self.url, token), now`). If the tunnel exits and
  a new one starts while the post is in flight (A, then "", then B), the topic holds A while `sent` says B, so no
  poll posts B until REPOST_SECS: the phone reads a dead address for up to 6 hours. `MiniApp.run` sleeps
  TUNNEL_RETRY_SECS (10 s) between the two tunnels and `urlopen`'s timeout is per socket operation (DNS not
  bounded), so a post that hangs while the network drops (which is also what kills a tunnel) can span it.
  `current_violates` is that trace; `current_stays_stale` and `current_posts_only_at_repost` show that no poll
  posts B until the 720th poll after A's post (tick 721), REPOST_SECS on.

  Finding 2 (the code at 2ad3550, P4, and so P1 and P3; FIXED). `publish_address` promises "False on any failure (logged,
  never raised)" but catches only `OSError`. Checked with the repo venv at 2ad3550: a server reply that breaks
  `http.client` (`http.client.BadStatusLine`, not an `OSError`) propagates out of `publish_address`, and a
  rendezvous base with no scheme (`EVE_LINK_RENDEZVOUS_URL=ntfy.sh`) raises `ValueError: unknown url type`
  on every post. miniapp.py:1073 awaits the post with no `try`, so the exception ends `_announce_loop`; the task
  was fire-and-forget (1085), so nothing logs it until `run()` cancels it. From then on nothing is posted again:
  the next tunnel's address never reaches the topic (P1), there is no retry (P3), and with the repost gone ntfy
  drops the last message after 12 hours. `current_dies` is that trace (`.raises`); `current_dead_forever` proves
  that after it, for EVERY later trace of events, the loop stays dead and every topic stays as it was.

  The fix, now in the code, has two parts: (1) record the address that was posted, `url = self.url` before the
  await and `sent = (url, token)` after (miniapp.py:1087-1090, `Fix.step .ok`); (2) a raising post counts as a
  failed one: `except Exception` in `publish_address` (stick_link.py:130-134) and, as a second guard, the `try`
  around the loop's iteration (miniapp.py:1084-1092); either alone gives `Fix.step .raises` = `.fail`, the loop
  sleeping and polling again with `sent` unchanged. Part 1 alone does not fix finding 2 (`url_fix_alone_dies`).
  The proofs of P1-P4 here are of the fixed code; this file proves nothing of P2 and P3 for the 2ad3550 code
  (whose `.fail` and polls are the same as the fix's: `common`). Replays against the real loop:
  bridge/tests/test_stick_link.py `test_the_announcer_records_the_address_it_posted_not_the_one_after` (`trace`),
  `test_a_raising_post_is_a_failed_post_and_the_announcer_goes_on` (`trace2`) and `test_publish_never_raises`.

  Abstractions, stated:
  - A quick tunnel's address is new each start, and a rotated token is new (`secrets.token_urlsafe(32)`): `up`
    and `rotate` hand out fresh numbers; 0 is "" (no address, no token).
  - A token can read as "" for one poll (`current()` failing to read the file) and come back: `wakeNoTok`.
  - Only this loop posts to the topic, and a topic of a token never used before is empty.
  - A post that returns True reached ntfy; one that returns False or raises did not. Excluded: a post that reached
    ntfy and still fails (a timeout after the server accepted the body). For the fix this only re-posts: the
    address is always new to `sent`, so the next poll posts it again; it is not modelled or proved here.
  - The rendezvous is on (`rendezvous_base()` non-empty, 1063-1066); off, the loop returns and posts nothing.
-/
namespace Eve.Rendezvous

/-- REPOST_SECS (stick_link.py:96), 6 h, in 30 s ticks. -/
def REPOST : Nat := 720
/-- ntfy.sh's message cache, 12 h, in 30 s ticks. -/
def CACHE : Nat := 1440

inductive Phase where
  | idle                                   -- at `await asyncio.sleep(every)` (miniapp.py:1075)
  | posting (u t ts : Nat)                 -- at `await asyncio.to_thread(publish_address, url, token, base)` (1073)
  | dead                                   -- the loop's task ended with an exception out of the post (1073, 1085)
  deriving DecidableEq, Repr

inductive Event where
  | up          -- `self.url = await tunnel.start(port)` (miniapp.py:1090): a new address
  | down        -- `self.url = ""` (miniapp.py:1100): the tunnel exited
  | rotate      -- `LinkToken.rotate` (stick_link.py:72-76): a new token in the file
  | wake        -- the loop's next iteration (1069-1072), `every` after the last
  | wakeNoTok   -- the same, but `current()` cannot read the file and returns "" (stick_link.py:57-58)
  | ok          -- the post returns True (stick_link.py:137)
  | fail        -- the post returns False (stick_link.py:127, 132, 135)
  | raises      -- the post raises an exception that is not an `OSError` (stick_link.py:129-130), e.g.
                --   `http.client.BadStatusLine`, or `ValueError` from a base URL with no scheme
  deriving DecidableEq, Repr

/-- A function updated at one point. -/
def upd (f : Nat → Nat) (k v : Nat) : Nat → Nat := fun x => if x = k then v else f x
/-- A two-argument function updated at one point. -/
def upd2 (f : Nat → Nat → Nat) (a b v : Nat) : Nat → Nat → Nat := fun x y => if x = a ∧ y = b then v else f x y

structure S where
  url : Nat := 0                           -- self.url (0 is "")
  nextUrl : Nat := 1                       -- the next quick tunnel's address
  tok : Nat := 0                           -- the token in the stick-link file (0: none)
  nextTok : Nat := 1
  now : Nat := 0                           -- clock(), in ticks
  phase : Phase := .idle
  sentUrl : Nat := 0                       -- `sent` (miniapp.py:1067)
  sentTok : Nat := 0
  sentAt : Nat := 0                        -- `sent_at` (1068)
  topic : Nat → Nat := fun _ => 0          -- ntfy: the newest address on each token's topic (0: none)
  topicAt : Nat → Nat := fun _ => 0        -- ntfy: when it was posted
  okAt : Nat → Nat → Nat := fun _ _ => 0   -- ghost: 1 + the time of the last successful post of (url, token)
  spam : Bool := false                     -- ghost: a successful post came less than REPOST after the last one
  dirty : Bool := false                    -- ghost: the address or the token changed since the last poll began
  postFailed : Bool := false               -- ghost: the last poll's post returned False
  readFailed : Bool := false               -- ghost: the last poll read the token as ""

/-- The condition of miniapp.py:1072: `self.url and token and ((self.url, token) != sent or now - sent_at >=
REPOST_SECS)`. -/
abbrev Wants (s : S) (tok now : Nat) : Prop :=
  s.url ≠ 0 ∧ tok ≠ 0 ∧ (¬(s.url = s.sentUrl ∧ tok = s.sentTok) ∨ s.sentAt + REPOST ≤ now)

/-- The topic is fresh: the newest message ntfy still holds on the token's topic is the current address. -/
def Fresh (s : S) : Prop :=
  s.phase = .idle → s.dirty = false → s.postFailed = false → s.readFailed = false → s.url ≠ 0 → s.tok ≠ 0 →
    s.topic s.tok = s.url ∧ s.now < s.topicAt s.tok + CACHE

def NoSpam (s : S) : Prop := s.spam = false

def Retry (s : S) : Prop :=
  s.phase = .idle → s.postFailed = true → s.dirty = false → Wants s s.tok (s.now + 1)

/-- (P4) The loop's task is still running. -/
def Alive (s : S) : Prop := s.phase ≠ .dead

def Spec (s : S) : Prop := Fresh s ∧ NoSpam s ∧ Retry s ∧ Alive s

/-- One iteration of the loop with the token read (2ad3550 miniapp.py:1070-1073; fixed 1085-1089): `now = clock()`,
the condition, and the post, which carries `self.url` and `token` as they are now (fixed: `url = self.url`). -/
def poll (s : S) : S :=
  match s.phase with
  | .idle =>
      if Wants s s.tok (s.now + 1) then
        { s with now := s.now + 1, dirty := false, postFailed := false, readFailed := false,
                 phase := .posting s.url s.tok (s.now + 1) }
      else { s with now := s.now + 1, dirty := false, postFailed := false, readFailed := false }
  | .posting .. => s
  | .dead => s

/-- The same iteration when `current()` returns "": `token` is falsy, so no post (1072). -/
def pollNoTok (s : S) : S :=
  match s.phase with
  | .idle => { s with now := s.now + 1, dirty := false, postFailed := false, readFailed := true }
  | .posting .. => s
  | .dead => s

/-- The post of `u` to `t`'s topic, made at `ts`, reached ntfy (stick_link.py:129, 137). -/
def land (s : S) (u t ts : Nat) : S :=
  { s with topic := upd s.topic t u, topicAt := upd s.topicAt t ts, okAt := upd2 s.okAt u t (ts + 1),
           spam := s.spam || decide (0 < s.okAt u t ∧ ts + 1 < s.okAt u t + REPOST), phase := .idle }

/-- The events that do not differ between the current code and the fix. -/
def common (s : S) : Event → S
  | .up => { s with url := s.nextUrl, nextUrl := s.nextUrl + 1, dirty := true }
  | .down => { s with url := 0, dirty := true }
  | .rotate => { s with tok := s.nextTok, nextTok := s.nextTok + 1, dirty := true }
  | .wake => poll s
  | .wakeNoTok => pollNoTok s
  -- `if await ...:` false: `sent` and `sent_at` stay (2ad3550 1073-1074; fixed 1089-1090).
  | .fail => match s.phase with
      | .posting .. => { s with phase := .idle, postFailed := true }
      | .idle => s
      | .dead => s
  | .ok => s
  | .raises => s

/-! ## The code at 2ad3550 -/

def Cur.step (s : S) : Event → S
  -- miniapp.py:1074: `sent, sent_at = (self.url, token), now`, with `self.url` read after the await.
  | .ok => match s.phase with
      | .posting u t ts => { land s u t ts with sentUrl := s.url, sentTok := t, sentAt := ts }
      | .idle => s
      | .dead => s
  -- miniapp.py:1073: no `try` around the await, so the exception leaves `_announce_loop` and ends its task.
  | .raises => match s.phase with
      | .posting .. => { s with phase := .dead, postFailed := true }
      | _ => s
  | e => common s e

def Cur.run (evs : List Event) : S := evs.foldl Cur.step {}

/-- A token, address A (1), a poll that posts A; while the post is in flight the tunnel restarts as B (2); the post
returns True and `sent` records B. The next poll sees (B, token) == sent and posts nothing: the topic says A. -/
def trace : List Event := [.rotate, .up, .wake, .down, .up, .ok, .wake]

theorem current_violates :
    let s := Cur.run trace
    s.phase = .idle ∧ s.dirty = false ∧ s.postFailed = false ∧ s.readFailed = false ∧
      s.url = 2 ∧ s.tok = 1 ∧ s.topic 1 = 1 ∧ s.sentUrl = 2 ∧ ¬ Fresh s := by
  refine ⟨by decide, by decide, by decide, by decide, by decide, by decide, by decide, by decide, ?_⟩
  intro h
  have := (h (by decide) (by decide) (by decide) (by decide) (by decide) (by decide)).1
  exact absurd this (by decide)

set_option maxRecDepth 100000 in
/-- The stale address stays: every poll up to tick 720 posts nothing, so the phone reads A (a dead address) for
REPOST_SECS. -/
theorem current_stays_stale :
    let s := (List.replicate 718 Event.wake).foldl Cur.step (Cur.run trace)
    s.phase = .idle ∧ s.now = 720 ∧ s.url = 2 ∧ s.topic s.tok = 1 := by
  decide

set_option maxRecDepth 100000 in
/-- B goes out only with the repost, at tick 721: REPOST_SECS after A was posted. -/
theorem current_posts_only_at_repost :
    let s := (List.replicate 719 Event.wake).foldl Cur.step (Cur.run trace)
    s.phase = .posting 2 1 721 := by
  decide

/-- Finding 2: a token, address A, a poll that posts A, and the post raises (`BadStatusLine`, or `ValueError` from
a base with no scheme). The exception leaves the loop: its task is dead. -/
def trace2 : List Event := [.rotate, .up, .wake, .raises]

theorem current_dies : (Cur.run trace2).phase = .dead ∧ ¬ Alive (Cur.run trace2) :=
  ⟨by decide, fun h => h (by decide)⟩

/-- A dead loop stays dead and posts nothing, whatever happens next. -/
theorem cur_dead_step (s : S) (h : s.phase = .dead) (e : Event) :
    (Cur.step s e).phase = .dead ∧ (Cur.step s e).topic = s.topic := by
  obtain ⟨url, nextUrl, tok, nextTok, now, phase, sentUrl, sentTok, sentAt, topic, topicAt, okAt, spam, dirty,
    postFailed, readFailed⟩ := s
  simp only at h
  subst h
  cases e <;> simp [Cur.step, common, poll, pollNoTok]

theorem cur_dead_run (evs : List Event) :
    ∀ s : S, s.phase = .dead → (evs.foldl Cur.step s).phase = .dead ∧ (evs.foldl Cur.step s).topic = s.topic := by
  induction evs with
  | nil => intro s h; exact ⟨h, rfl⟩
  | cons e rest ih =>
    intro s h
    obtain ⟨d1, d2⟩ := cur_dead_step s h e
    obtain ⟨r1, r2⟩ := ih _ d1
    exact ⟨r1, r2.trans d2⟩

/-- After finding 2's trace, for EVERY later trace (tunnel restarts, rotations, polls, post results): the loop
stays dead, no topic ever changes (the token's topic stays empty: A's post raised, and no address after it is
ever posted, whatever the tunnel's address now is), and P4 fails. -/
theorem current_dead_forever (evs : List Event) :
    let s := evs.foldl Cur.step (Cur.run trace2)
    s.phase = .dead ∧ s.topic = (Cur.run trace2).topic ∧ s.topic 1 = 0 ∧ ¬ Alive s := by
  obtain ⟨d1, d2⟩ := cur_dead_run evs (Cur.run trace2) (by decide)
  refine ⟨d1, d2, ?_, fun h => h d1⟩
  rw [d2]; decide

/-! ## The fixed code (fix/lean-findings) -/

def Fix.step (s : S) : Event → S
  -- miniapp.py:1087 `url, now = self.url, clock()` before the await, 1090 `sent, sent_at = (url, token), now`.
  | .ok => match s.phase with
      | .posting u t ts => { land s u t ts with sentUrl := u, sentTok := t, sentAt := ts }
      | .idle => s
      | .dead => s
  -- stick_link.py:130-134 `except Exception` → False, and miniapp.py:1091-1092 `except Exception` around the
  -- iteration: a raising post is a failed post, `sent` stays, and the loop goes on to its sleep (1093).
  | .raises => common s .fail
  | e => common s e

def Fix.run (evs : List Event) : S := evs.foldl Fix.step {}

/-- Part 1 of the fix alone (record the posted address) with the current handling of a raising post. -/
def UrlFix.step (s : S) : Event → S
  | .raises => Cur.step s .raises
  | e => Fix.step s e

/-- Part 1 alone does not save the loop: finding 2's trace still kills it. -/
theorem url_fix_alone_dies : ¬ Alive (trace2.foldl UrlFix.step {}) := fun h => h (by decide)

/-! ### Proof: an inductive invariant, preserved by every event from every state. -/

/-- `omega` after reducing the structure projections. -/
macro "lomega" : tactic => `(tactic| ((try dsimp only at *) <;> omega))
/-- The phase is not `dead`: kept from the old state, or a new `idle`/`posting`. -/
macro "alive_tac" : tactic => `(tactic| first | assumption | (dsimp only; assumption) | simp [Alive] | simp_all [Alive])

structure Inv (s : S) : Prop where
  url_new : s.url = 0 ∨ s.url + 1 = s.nextUrl
  tok_new : s.tok = 0 ∨ s.tok + 1 = s.nextTok
  sent_lt : s.sentUrl < s.nextUrl ∧ s.sentTok < s.nextTok
  sent_le_url : s.url ≠ 0 → s.sentUrl ≤ s.url
  sent_le_tok : s.tok ≠ 0 → s.sentTok ≤ s.tok
  topic_sent : s.sentTok ≠ 0 → s.topic s.sentTok = s.sentUrl ∧ s.topicAt s.sentTok = s.sentAt
  ok_le : ∀ u t, s.okAt u t ≠ 0 → u ≤ s.sentUrl ∧ t ≤ s.sentTok
  ok_sent : s.okAt s.sentUrl s.sentTok ≠ 0 → s.okAt s.sentUrl s.sentTok = s.sentAt + 1
  idle_sent : s.phase = .idle → s.dirty = false → s.postFailed = false → s.readFailed = false → s.url ≠ 0 →
    s.tok ≠ 0 → s.url = s.sentUrl ∧ s.tok = s.sentTok ∧ s.now < s.sentAt + REPOST
  posting : ∀ u t ts, s.phase = .posting u t ts →
    ts = s.now ∧ u ≠ 0 ∧ t ≠ 0 ∧ (s.dirty = false → s.url = u ∧ s.tok = t) ∧ u < s.nextUrl ∧ t < s.nextTok ∧
    (s.url ≠ 0 → u ≤ s.url) ∧ (s.tok ≠ 0 → t ≤ s.tok) ∧ s.sentUrl ≤ u ∧ s.sentTok ≤ t ∧
    (¬(u = s.sentUrl ∧ t = s.sentTok) ∨ s.sentAt + REPOST ≤ ts) ∧
    (s.okAt u t ≠ 0 → s.okAt u t + REPOST ≤ ts + 1) ∧ s.postFailed = false
  retry : Retry s
  nospam : s.spam = false
  alive : Alive s

theorem inv_init : Inv ({} : S) := by
  constructor <;> simp [Retry, Alive]

theorem inv_step (s : S) (e : Event) (h : Inv s) : Inv (Fix.step s e) := by
  obtain ⟨url, nextUrl, tok, nextTok, now, phase, sentUrl, sentTok, sentAt, topic, topicAt, okAt, spam, dirty,
    postFailed, readFailed⟩ := s
  obtain ⟨h1, h2, h3, h4, h5, h6, h7, h8, h9, h10, h11, h12, h13⟩ := h
  simp only [Alive] at h1 h2 h3 h4 h5 h6 h7 h8 h9 h10 h11 h12 h13
  cases e with
  | up =>
    simp only [Fix.step, common]
    refine ⟨Or.inr rfl, h2, ⟨by lomega, h3.2⟩, fun _ => by lomega, h5, h6, h7, h8, ?_, ?_, ?_, h12, by alive_tac⟩
    · intro _ hd; simp at hd
    · intro u t ts hp
      obtain ⟨a1, a2, a3, _, a5, a6, _, a8, a9, a10, a11, a12, a13⟩ := h10 u t ts hp
      exact ⟨a1, a2, a3, fun hd => by simp at hd, by lomega, a6, fun _ => by lomega, a8, a9, a10, a11, a12, a13⟩
    · intro _ _ hd; simp at hd
  | down =>
    simp only [Fix.step, common]
    refine ⟨Or.inl rfl, h2, h3, fun h => absurd rfl h, h5, h6, h7, h8, ?_, ?_, ?_, h12, by alive_tac⟩
    · intro _ hd; simp at hd
    · intro u t ts hp
      obtain ⟨a1, a2, a3, _, a5, a6, _, a8, a9, a10, a11, a12, a13⟩ := h10 u t ts hp
      exact ⟨a1, a2, a3, fun hd => by simp at hd, a5, a6, fun h => absurd rfl h, a8, a9, a10, a11, a12, a13⟩
    · intro _ _ hd; simp at hd
  | rotate =>
    simp only [Fix.step, common]
    refine ⟨h1, Or.inr rfl, ⟨h3.1, by lomega⟩, h4, fun _ => by lomega, h6, h7, h8, ?_, ?_, ?_, h12, by alive_tac⟩
    · intro _ hd; simp at hd
    · intro u t ts hp
      obtain ⟨a1, a2, a3, _, a5, a6, a7, _, a9, a10, a11, a12, a13⟩ := h10 u t ts hp
      exact ⟨a1, a2, a3, fun hd => by simp at hd, a5, by lomega, a7, fun _ => by lomega, a9, a10, a11, a12, a13⟩
    · intro _ _ hd; simp at hd
  | wakeNoTok =>
    cases phase with
    | dead => exact absurd rfl h13
    | posting u t ts =>
      simp only [Fix.step, common, pollNoTok]
      exact ⟨h1, h2, h3, h4, h5, h6, h7, h8, h9, h10, h11, h12, by alive_tac⟩
    | idle =>
      simp only [Fix.step, common, pollNoTok]
      refine ⟨h1, h2, h3, h4, h5, h6, h7, h8, ?_, ?_, ?_, h12, by alive_tac⟩
      · intro _ _ _ hr; simp at hr
      · intro u t ts hp; cases hp
      · intro _ hf; simp at hf
  | wake =>
    cases phase with
    | dead => exact absurd rfl h13
    | posting u t ts =>
      simp only [Fix.step, common, poll]
      exact ⟨h1, h2, h3, h4, h5, h6, h7, h8, h9, h10, h11, h12, by alive_tac⟩
    | idle =>
      simp only [Fix.step, common, poll]
      split
      · rename_i hw
        simp only [Wants] at hw
        refine ⟨h1, h2, h3, h4, h5, h6, h7, h8, ?_, ?_, ?_, h12, by alive_tac⟩
        · intro hp; cases hp
        · intro u t ts hp
          cases hp
          obtain ⟨hu, ht, hc⟩ := hw
          refine ⟨rfl, hu, ht, fun _ => ⟨rfl, rfl⟩, by lomega, by lomega, fun _ => Nat.le_refl _,
            fun _ => Nat.le_refl _, h4 hu, h5 ht, hc, ?_, rfl⟩
          intro hne
          obtain ⟨l1, l2⟩ := h7 url tok hne
          have e1 : url = sentUrl := by have := h4 hu; omega
          have e2 : tok = sentTok := by have := h5 ht; omega
          subst e1 e2
          have := h8 hne
          rcases hc with hc | hc
          · exact absurd ⟨rfl, rfl⟩ hc
          · lomega
        · intro hp; cases hp
      · rename_i hw
        simp only [Wants] at hw
        refine ⟨h1, h2, h3, h4, h5, h6, h7, h8, ?_, ?_, ?_, h12, by alive_tac⟩
        · intro _ _ _ _ hu ht
          have : url = sentUrl ∧ tok = sentTok ∧ ¬ sentAt + REPOST ≤ now + 1 := by
            by_cases hs : url = sentUrl ∧ tok = sentTok
            · exact ⟨hs.1, hs.2, fun hr => hw ⟨hu, ht, Or.inr hr⟩⟩
            · exact absurd ⟨hu, ht, Or.inl hs⟩ hw
          refine ⟨this.1, this.2.1, ?_⟩
          have := this.2.2
          lomega
        · intro u t ts hp; cases hp
        · intro _ hf; simp at hf
  | ok =>
    cases phase with
    | dead => exact absurd rfl h13
    | idle =>
      simp only [Fix.step]
      exact ⟨h1, h2, h3, h4, h5, h6, h7, h8, h9, h10, h11, h12, by alive_tac⟩
    | posting u t ts =>
      obtain ⟨a1, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11, a12, a13⟩ := h10 u t ts rfl
      simp only [Fix.step, land]
      refine ⟨h1, h2, ⟨a5, a6⟩, a7, a8, ?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_⟩
      · intro _; simp [upd]
      · intro u' t' hne
        by_cases hc : u' = u ∧ t' = t
        · lomega
        · simp only [upd2, hc, ite_false] at hne
          have := h7 u' t' hne
          lomega
      · intro _; simp [upd2]
      · intro _ hd _ _ _ _
        obtain ⟨e1, e2⟩ := a4 hd
        refine ⟨e1, e2, ?_⟩
        unfold REPOST; lomega
      · intro u' t' ts' hp; cases hp
      · intro _ hf; simp [a13] at hf
      · simp only [h12, Bool.false_or, decide_eq_false_iff_not]
        intro ⟨c1, c2⟩
        have := a12 (by lomega)
        lomega
      · simp [Alive]
  | fail | raises =>
    cases phase with
    | dead => exact absurd rfl h13
    | idle =>
      simp only [Fix.step, common]
      exact ⟨h1, h2, h3, h4, h5, h6, h7, h8, h9, h10, h11, h12, by alive_tac⟩
    | posting u t ts =>
      obtain ⟨a1, a2, a3, a4, _, _, _, _, _, _, a11, _, _⟩ := h10 u t ts rfl
      simp only [Fix.step, common]
      refine ⟨h1, h2, h3, h4, h5, h6, h7, h8, ?_, ?_, ?_, h12, by alive_tac⟩
      · intro _ _ hf; simp at hf
      · intro u' t' ts' hp; cases hp
      · intro _ _ hd
        obtain ⟨e1, e2⟩ := a4 hd
        subst e1 e2
        refine ⟨a2, a3, ?_⟩
        rcases a11 with c | c
        · exact Or.inl c
        · exact Or.inr (by lomega)

theorem inv_run (evs : List Event) : ∀ s : S, Inv s → Inv (evs.foldl Fix.step s) := by
  induction evs with
  | nil => intro s h; exact h
  | cons e rest ih => intro s h; exact ih _ (inv_step s e h)

theorem spec_of_inv (s : S) (h : Inv s) : Spec s := by
  refine ⟨?_, h.nospam, h.retry, h.alive⟩
  intro hi hd hf hr hu ht
  obtain ⟨e1, e2, hn⟩ := h.idle_sent hi hd hf hr hu ht
  have ht' : s.sentTok ≠ 0 := by rw [← e2]; exact ht
  obtain ⟨t1, t2⟩ := h.topic_sent ht'
  rw [e2, e1, t1, t2]
  refine ⟨rfl, ?_⟩
  unfold REPOST at hn; unfold CACHE; omega

/-- For every trace of tunnel restarts, token rotations, unreadable tokens, polls and post results: (P1) once a
poll that began after the last change has posted successfully, the topic's newest message is the current address
and ntfy still holds it; (P2) no (address, token) is posted twice within REPOST_SECS; (P3) after a failed post with
nothing changed, the next poll posts, whether it returned False or raised; (P4) the loop never dies. -/
theorem fixed_invariant (evs : List Event) : Spec (Fix.run evs) :=
  spec_of_inv _ (inv_run evs {} inv_init)

/-- (P3) as a step: after a failed post (False, or raised and caught by part 2 of the fix), the very next poll
posts the same address to the same token. -/
theorem fixed_retries (evs : List Event) (hi : (Fix.run evs).phase = .idle)
    (hf : (Fix.run evs).postFailed = true) (hd : (Fix.run evs).dirty = false) :
    (Fix.step (Fix.run evs) .wake).phase =
      .posting (Fix.run evs).url (Fix.run evs).tok ((Fix.run evs).now + 1) := by
  have hw := (fixed_invariant evs).2.2.1 hi hf hd
  generalize Fix.run evs = s at hi hw ⊢
  obtain ⟨url, nextUrl, tok, nextTok, now, phase, sentUrl, sentTok, sentAt, topic, topicAt, okAt, spam, dirty,
    postFailed, readFailed⟩ := s
  simp only at hi
  subst hi
  simp only [Fix.step, common, poll]
  split
  · rfl
  · rename_i hn; exact absurd hw hn

/-- The trace that breaks the current code, on the fixed one: `sent` records A, so the next poll posts B. -/
theorem fixed_replays_counterexample :
    (Fix.run trace).phase = .posting 2 1 2 := by
  decide

end Eve.Rendezvous
