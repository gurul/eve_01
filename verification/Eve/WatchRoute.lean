/-
  Where "/watch <words>" goes in the Telegram inlet (bridge/src/eve_bridge/telegram.py, `_handle`).

  "/watch AAPL below 300" is eve's own command: the bot menu offers it (`BOT_COMMANDS`, "watch"), and
  `_handle` rewrites it to "Watch AAPL below 300" for the text brain, which holds the watch tools
  (telegram.py:1871-1875). Bare "/watch" is one of eve's code words and is answered by code "relay or not"
  (1943-1944, 1962-1964, 1986-1990). The property:

    a "/watch <words>" request is never typed into the Claude session or sent to Codex; it is handled
    exactly as "eve: watch <words>" is (the brain, or the answer to a question that takes any text).

  Abstraction. The relay is off, Claude (`self.claude`) or Codex (`self._codex_chat` is this chat); the two
  are exclusive (`codex on` clears `self.claude`, 1893-1897; `claude on` clears the Codex chat, 1912-1919).
  The question slot (`_pending_answer`, `_pending_strict`, `_pending_words`; see Questions.lean for the slot
  itself) holds one kind: none, `free` (a task's question: any text answers it), `choice` (a prompt with
  buttons from `_ask_user`: strict) or `perm` (`decide_permission`'s Allow/Deny: strict, and a permission).
  A message is `plain` words, `yes` (a bare yes/no or a button's word, `consent.bare_decision`), `bareWatch`
  ("/watch", in WATCH_WORDS), `watchCmd` ("/watch <words>") or `evePre` ("eve: <words>", EVE_PREFIX).
  After the rewrite "Watch <words>" is not a yes/no and not a code word, so the current code routes it as
  `plain`. The other branches of `_handle` (launch tree, rundown, codex/claude commands, stop words, images)
  are not reached by these five messages and are left out.

  Bound: the step lemma is checked by `decide` on every one of the 3 x 4 x 2 = 24 states and 13 events
  (8 control events, `send` of each of the 5 messages); `fixed_invariant` then holds for every trace of any
  length, by induction. `fix_agrees` checks the fix changes nothing but "/watch <words>", on all 12 x 5
  (state, message) pairs.
-/
namespace Eve.WatchRoute

inductive Relay where
  | off
  | claude
  | codex
  deriving DecidableEq, Repr

inductive Pending where
  | none
  | free
  | choice
  | perm
  deriving DecidableEq, Repr

inductive Msg where
  | plain
  | yes
  | bareWatch
  | watchCmd
  | evePre
  deriving DecidableEq, Repr

inductive Dest where
  | brain       -- `_turn`, telegram.py:2017
  | listing     -- the /watches listing by code, 1986-1990
  | answer      -- the waiting question's future, 2003-2013
  | claude      -- `_type_to_claude`, 1969-1974
  | codex       -- `_codex_send`, 1947-1952
  deriving DecidableEq, Repr

inductive Event where
  | claudeOn
  | claudeOff
  | codexOn
  | codexOff
  | ask          -- `_ask_user` with no choices: a free question
  | askChoice    -- `_ask_user` with choices: a prompt with buttons
  | perm         -- `decide_permission`
  | settle       -- a tap, a timeout, or the asker gone
  | send (m : Msg)
  deriving DecidableEq, Repr

structure S where
  relay : Relay := .off
  pending : Pending := .none
  lost : Bool := false         -- ghost: a "/watch <words>" went into a relay
  deriving DecidableEq, Repr

def Spec (s : S) : Bool := !s.lost

def strict (p : Pending) : Bool := p == .choice || p == .perm      -- `_pending_strict`

/-- `_answers_pending`, telegram.py:1803-1822. -/
def answersPending (s : S) (m : Msg) : Bool :=
  match s.pending with
  | .none => false                                   -- 1812-1813
  | .free => true                                    -- 1814-1815
  | .choice => m == .yes || s.relay == .off          -- 1818-1819, then 1822
  | .perm => m == .yes                               -- 1818-1819, then 1820-1821

/-- The code words (STEALTH, APPS, SPEND, WATCH_WORDS, SCREEN_NOW), 1944-1945 and 1962-1964. -/
def codeWord (m : Msg) : Bool := m == .bareWatch

/-- `_handle` from 1936 on. `exempt` is the fix: a "/watch <words>" skips both relay branches, as a code
word does, and goes on to the answer check and the brain. The current code has `exempt = false`. -/
def route (exempt : Bool) (s : S) (m : Msg) : Dest :=
  let isWatch := exempt && m == .watchCmd
  -- 1943-1955: the Codex relay takes anything that is not a code word and not the answer.
  let codexTakes := s.relay == .codex && !codeWord m && !isWatch && !answersPending s m
  if codexTakes && m != .evePre then .codex
  else
    -- 1953-1955: "eve: ..." lost its prefix on the way and is addressed to eve.
    let addressed := codexTakes && m == .evePre
    let m' := if codexTakes then Msg.plain else m
    -- 1962-1977: the Claude relay likewise (never both: the relays are exclusive).
    let claudeTakes := !codeWord m' && s.relay == .claude && !isWatch && !answersPending s m'
    if claudeTakes && m' != .evePre then .claude
    else
      let addressed := addressed || (claudeTakes && m' == .evePre)
      let m'' := if claudeTakes then Msg.plain else m'
      if codeWord m'' then .listing                                      -- 1986-1990
      else if !(addressed && strict s.pending) && answersPending s m'' then .answer   -- 2003-2013
      else .brain                                                        -- 2017

def step (exempt : Bool) (s : S) : Event → S
  -- `claude on` (1911-1921, `_claude_on`): the Codex chat, if any, is disconnected first.
  | .claudeOn => { s with relay := .claude }
  -- `claude off` (1923-1930): `_defer_permission` settles a permission prompt, then the relay is off.
  | .claudeOff => if s.relay == .claude then
      { s with relay := .off, pending := if s.pending == .perm then .none else s.pending } else s
  -- `codex on` / `codex use` (1890-1897): `_defer_permission`, then `self.claude` is cleared.
  | .codexOn => { s with relay := .codex, pending := if s.pending == .perm then .none else s.pending }
  | .codexOff => if s.relay == .codex then { s with relay := .off } else s
  | .ask => { s with pending := .free }
  | .askChoice => { s with pending := .choice }
  -- `decide_permission` (2889-2892): only with the Claude relay on and nothing awaited.
  | .perm => if s.relay == .claude && s.pending == .none then { s with pending := .perm } else s
  | .settle => { s with pending := .none }
  | .send m =>
      let d := route exempt s m
      { s with pending := if d == .answer then .none else s.pending,
               lost := s.lost || (m == .watchCmd && (d == .claude || d == .codex)) }

/-! ## The current code -/

def Cur.run (evs : List Event) : S := evs.foldl (step false) {}

/-- The Claude relay is on and the owner types "/watch AAPL below 300": it is typed into the session. -/
theorem current_violates :
    let s := Cur.run [.claudeOn, .send .watchCmd]
    route false { relay := .claude } .watchCmd = .claude ∧ s.lost = true ∧ Spec s = false := by
  decide

/-- The same with the Codex relay: it is sent to Codex. -/
theorem current_violates_codex :
    let s := Cur.run [.codexOn, .send .watchCmd]
    route false { relay := .codex } .watchCmd = .codex ∧ Spec s = false := by
  decide

/-! ## The fixed code

  `_handle` (fixed): `and not watch_ask` in the Codex branch's condition at 1943 and in the Claude branch's
  at 1965. Nothing else changes. -/

def Fix.run (evs : List Event) : S := evs.foldl (step true) {}

/-- The fixed route never hands "/watch <words>" to a relay, in any state. -/
theorem fix_route_keeps_watch : ∀ (r : Relay) (p : Pending) (l : Bool),
    route true ⟨r, p, l⟩ .watchCmd ≠ .claude ∧ route true ⟨r, p, l⟩ .watchCmd ≠ .codex := by
  intro r p l; cases r <;> cases p <;> cases l <;> decide

theorem inv_step : ∀ (s : S) (e : Event), Spec s = true → Spec (step true s e) = true := by
  intro s e
  rcases s with ⟨r, p, l⟩
  cases r <;> cases p <;> cases l <;> (try cases e) <;> (try rename_i m; cases m) <;> decide

theorem inv_run (evs : List Event) : ∀ s : S, Spec s = true → Spec (evs.foldl (step true) s) = true := by
  induction evs with
  | nil => intro s h; exact h
  | cons e rest ih => intro s h; exact ih _ (inv_step s e h)

/-- For every trace of relay switches, questions and messages: no "/watch <words>" ever reaches Claude
Code or Codex. -/
theorem fixed_invariant (evs : List Event) : Spec (Fix.run evs) = true :=
  inv_run evs {} (by decide)

/-- "/watch <words>" is routed exactly as "eve: watch <words>", in every state. -/
theorem fix_is_eve_prefix : ∀ (r : Relay) (p : Pending),
    route true ⟨r, p, false⟩ .watchCmd = route true ⟨r, p, false⟩ .evePre := by
  intro r p; cases r <;> cases p <;> decide

/-- The fix changes nothing else: every other message goes where it went before, in every state. -/
theorem fix_agrees : ∀ (r : Relay) (p : Pending) (m : Msg), m ≠ .watchCmd →
    route true ⟨r, p, false⟩ m = route false ⟨r, p, false⟩ m := by
  intro r p m h; cases r <;> cases p <;> cases m <;> first | decide | exact absurd rfl h

/-- The traces that break the current code, on the fixed one: the brain gets it. -/
theorem fixed_replays_counterexample :
    route true { relay := .claude } .watchCmd = .brain ∧ route true { relay := .codex } .watchCmd = .brain ∧
      (Fix.run [.claudeOn, .send .watchCmd]).lost = false := by
  decide

end Eve.WatchRoute
