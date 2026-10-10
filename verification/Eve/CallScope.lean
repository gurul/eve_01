/-
  Eve.CallScope — which lines eve reads out on a call: only the answers to work that began with words heard on
  that call, never the answer to a message typed in the chat while a call happens to be open.

  Four machines over one set of events:
  - `Old`: commit 48df362, the parent of 1c68787 that added `_FROM_CALL`. There `_say` (telegram.py:2449),
    `_send_choices` (2517), `_think` (3551, 3563-3565, 3577) and `_to_call` (4143) all read `self._call_say`
    directly.
  - `Cur`: commit 2ad3550, the code the findings were proved on. Its line numbers are those of 2ad3550:
    - `_FROM_CALL` (telegram.py:700), a Bool ContextVar that marks work begun on a call;
    - `TelegramInlet.hear` / `_hear` (telegram.py:4122-4151), which set it to True around the heard words;
    - `_call_reader` (telegram.py:4099-4101), `self._call_say if _FROM_CALL.get() else None`;
    - `_say` (2471-2488), `_send_choices` (2540-2551), `_think` (3580-3612, `call_say` fixed for the turn and
      handed to `SentenceStream`), `_to_call` (4203-4214), `listen` (4103-4117), `_spawn` (2044-2052);
    - the Claude relay: `_type_to_claude` (3134, spawned by `_handle`, 2284-2292); `relay_text` (3245, from
      daemon.py:3048 `asyncio.create_task`), `relay_notification` (3283, from daemon.py:2555), `relay_tool_call`'s
      AskUserQuestion (3232, from daemon.py:2592/2610), `decide_permission` (3291, awaited by the daemon's hook
      handlers at daemon.py:2567/2642/2662), each reaching `relay_line` (3189) → `_spawn(_flush_relay())` (3203)
      → `_say` / `_send_choices`, always in a daemon task's context;
    - daemon.py:2491 `_ipc_chat`: `tg.hear(text)` for words typed in the Mac chat window;
    - the "claude on" queue: `_handle` holds messages (2161-2163), `_claude_on_listed` routes them (3080-3082).
  - `Mid`: commit 1dd0f26, the first fix (branch fix/lean-findings, 2026-10-04). Its line numbers are those of
    the files at 1dd0f26:
    - telegram.py:716-724 `_FROM_CALL` holds the reader of the call the work began on (None: not begun on a call);
    - telegram.py:4177-4187 `hear(…, on_call=True)`: `_FROM_CALL.set(self._call_say if on_call else None)`;
    - telegram.py:4151-4156 `_call_reader`: the listener only if `_FROM_CALL` is set and that same call (`==`)
      still holds it;
    - telegram.py:3630, 3642-3647 `_think`: each streamed sentence goes through `_to_call` (4260-4270), so the
      listener is read at each line, as the model reads it;
    - telegram.py:3191 `_type_to_claude` saves `_relay_from_call = _FROM_CALL.get()` once the prompt went in;
      3246 `_flush_relay`, 3331-3335 `relay_notification` and 3371-3376 `decide_permission` read with it;
    - daemon.py:2492 `_ipc_chat`: `tg.hear(text, on_call=False)`;
    - telegram.py:1746, 2191 a held message is stored with its own `_FROM_CALL`; 3109-3117 `_claude_on_listed`
      replays each under that origin.
  - `Fix`: the fixed code after 1dd0f26; its lines are listed at its section below. `hear` takes the hearing
    call's reader (`call=`), and the relay keeps a FIFO of the prompts typed, advanced at Claude's turn boundary.

  The owner's requirement (2026-10-04): "dont read aloud things from telegram unless its something explicit from
  like the action call". The properties, over every trace of events:
  - (P1) a line is read on a call only if the work that produced it began with words heard on a call;
  - (P2) work that began with words heard on a call, and says a line while that call holds the chat's listener,
    has the line read out;
  - (P3) a line is read only on the call that heard the work's words (`crossCall` stays false).
  "Work" is a job: an asyncio task and everything it spawns, and also the Claude session's answer to a prompt that
  job typed into it. Its origin is a ghost (`spoken`, `call`).

  Scope of "work", stated. A line that answers words heard on a call reaches the chat in one of two ways: from the
  job's own task tree (modelled by `spawn` and `reply`), or from the Claude session the job typed the words into
  (modelled by `forward`, `turnBegin` and `relay` / `line`: in `Old` and `Cur` a relay line is attributed to the
  last prompt typed into the session; in `Mid` and `Fix` it answers the prompt of the turn Claude is in, `answering`,
  which Claude Code takes from its queue of typed prompts in order). Lines
  from eve's long-lived loops — the watcher's and the meet notifier's `tell_owner`, and the daemon's other hooks —
  are not the answer to words heard; they are modelled as new work not begun on a call (`typed`), so P1 keeps them
  off the call and P2 does not ask for them. A watch added by voice whose alert fires later is, under this scope,
  not "work begun on the call".

  Findings, in order:
  - Old (48df362): a line was read whenever a call held the listener. A message typed in Telegram during a call
    had its answer read aloud (`old_violates`). 1c68787 fixed that: `Cur` keeps P1 for every trace of core events
    (`current_p1_invariant`). Replay: `test_a_typed_message_is_not_read_aloud_on_an_open_call`.
  - Cur (2ad3550), five findings (`current_violates`, `current_gaps`):
    1. the Claude relay runs in a daemon task, where `_FROM_CALL` is False, so Claude's answer to a prompt spoken on
       the call is never read on it (P2 fails; P2 holds for every trace with no relay line,
       `current_p2_without_relay`);
    2. cross-call: `_FROM_CALL` is a Bool, not the call, so work outliving its call is read on the next one (P3);
    3. the Mac chat window goes through `tg.hear`, so typed words are read (P1);
    4. the "claude on" queue replays held messages with the claude-on task's flag, not their own (P1 and P2);
    5. the stick link's doc (removed 2026-10-07) and docs/stackchan/miniapp.md described the old reading of relay lines.
  - Mid (1dd0f26): (1)-(5) fixed, under the abstraction that the call holding the listener heard the words and
    that a relay line answers the last prompt typed. An adversarial verifier found both abstractions false; with
    the hearing call (`heardOn`) and the prompt Claude is answering (`turnBegin`, `answering`) as ghosts:
    6. the hearing call: `hear` marked the work with the LISTENER's reader. A desk call's `listening` stays True
       after a phone call took the listener, so the desk's words were answered on the phone
       (`current_hearing_call`, P3). Replay: bridge/tests/test_desk_call_lean.py
       `test_words_heard_on_the_desk_are_read_on_the_desk_while_the_phone_holds_the_listener`.
    7. the relay read every line with the origin of the LAST prompt typed, at the flush (`current_relay_fifo`):
       a typed prompt's answer read aloud while a spoken prompt waited behind it (P1); the reverse lost the spoken
       answer (P2); a batch straddling two prompts was read whole by the second (P2); a turn typed at the Mac's
       keyboard was read as the spoken prompt's (P1). Replays in bridge/tests/test_telegram.py:
       `test_a_typed_prompts_answer_is_not_read_while_a_spoken_prompt_waits_behind_it`,
       `test_a_spoken_prompts_answer_is_read_while_a_typed_prompt_waits_behind_it`,
       `test_a_relay_batch_that_straddles_two_prompts_is_read_by_each_prompts_origin`.
  - Fix: all fixed. P1, P2 and P3 hold for every trace, the relay, the chat window, the queue, the hearing call,
    Claude's turns and batched relay lines included (`fixed_invariant`); each counterexample is answered right
    (`fixed_replays`). Replays of (1)-(4) against the real class,
    bridge/tests/test_telegram.py: `test_claudes_answer_to_a_prompt_spoken_on_the_call_is_read_on_it` (1),
    `test_work_begun_on_one_call_is_not_read_on_the_next` (2),
    `test_words_typed_in_the_mac_chat_window_are_not_read_on_an_open_call` (3),
    `test_a_typed_message_held_by_a_spoken_claude_on_is_not_read` and
    `test_words_said_while_a_typed_claude_on_lists_are_read` (4). The docs now describe the rule (5).

  Abstractions, stated:
  - Call ids are ghosts: `listen(say)` gives the listener a fresh id, `listen(None, owner=…)` by the holder clears
    it. In the fixed code a call's identity is its reader, compared with `==` (each `call.say` is a new bound
    method of its call object), so a fresh id per call assumes no call object takes the listener twice. Which of
    the desk and the phone holds it is Eve/DeskCall.lean's business.
  - `reply j` is one line job `j` says in the owner's chat: `_say` with no title, a question with choices, a
    streamed sentence, or "On it.". A progress line is never read and is left out. The model reads the listener at
    the line. In `Cur` the turn's `call_say` was fixed at the start of the turn for streamed sentences; in `Fix`
    every sentence goes through `_to_call`, which reads it live, as the model does.
  - `spawn j` is any task job `j` starts. It inherits the job's context and its origin.
  - `forward j` is job `j` typing a line into the Claude session. `relay` is one line from the session reaching
    the chat (any of the four paths), said at once. `line` and `flush` split the batched path: a line arrives,
    and `_flush_relay` later sends every waiting line. Residual, stated: the boundary is the UserPromptSubmit
    hook, so a line of turn N that the transcript tailer delivers after turn N+1's hook is read as N+1's; and
    with the hook absent no relay line is read at all (the safe side). docs/stackchan/telegram.md says so.
  - A held message's hold and its replay are one event: its origin is captured when it is held, and the replay
    under that origin starts a job just as a fresh message would.
-/
namespace Eve.CallScope

/-- One piece of work: an asyncio task and its context. -/
structure Job where
  flag : Bool          -- 2ad3550's `_FROM_CALL.get()` in this task's context (a Bool)
  spoken : Bool        -- ghost: the work began with words heard on a call
  call : Nat           -- ghost: the call that heard them (0: none)
  began : Nat := 0     -- the fixed code's `_FROM_CALL.get()`: the reader of the call the work began on (0: None)
  deriving DecidableEq, Repr

inductive Event where
  | typed              -- a Telegram text: `_dispatch` → `_handle`, from the poll's context (`_FROM_CALL` unset)
  | heard              -- `hear(text)`: the call's words, routed by `_hear` → `_handle`
  | callStart          -- `listen(say)`: a call takes the listener
  | callEnd            -- `listen(None, owner=say)` by the holder
  | spawn (j : Nat)    -- job j starts a task (`_spawn`, `ensure_future`): the context is copied
  | reply (j : Nat)    -- job j says a line in the owner's chat
  | forward (j : Nat)  -- job j types its words into the Claude session (`_type_to_claude`)
  | relay              -- a line from the Claude session reaches the chat, from a daemon task
  deriving DecidableEq, Repr

structure S where
  listener : Nat := 0          -- `_call_say`: 0 is None, otherwise the (ghost) id of the call holding it
  next : Nat := 1              -- ghost: the next call's id
  jobs : List Job := []
  prompt : Job := { flag := false, spoken := false, call := 0 }
                               -- the job that typed the last prompt into the Claude session; at 1dd0f26
                               -- `_relay_from_call` is this job's `began`
  queue : List Job := []       -- prompts typed into the session that Claude has not begun, oldest first: ghost
                               -- (Claude Code's own queue), and in the fixed code `_relay_prompts`
  answering : Job := { flag := false, spoken := false, call := 0 }
                               -- the prompt of the turn Claude is in (ghost); in the fixed code its `began` is
                               -- `_relay_origin`
  batch : List (Job × Nat) := []
                               -- relay lines waiting for `_flush_relay`: the prompt each answers (ghost), and the
                               -- reader it took when it arrived (the fixed code; 0 at 1dd0f26, which took none)
  readBad : Bool := false      -- ghost: a line was read whose work did not begin on a call (P1 broken)
  missed : Bool := false       -- ghost: call work said a line while its call held the listener, unread (P2 broken)
  crossCall : Bool := false    -- ghost: a line was read on a call other than the one that heard the work's words
  deriving DecidableEq, Repr

def P1 (s : S) : Bool := !s.readBad
def P2 (s : S) : Bool := !s.missed
def P3 (s : S) : Bool := !s.crossCall
def Spec (s : S) : Bool := P1 s && P2 s

/-- The events that do not depend on the reading rule: the same in all three machines. -/
def common (s : S) : Event → S
  -- `_dispatch` → `_handle`: the poll loop's context, where `_FROM_CALL` is its default (False; in Fix, None).
  | .typed => { s with jobs := s.jobs ++ [{ flag := false, spoken := false, call := 0 }] }
  | .heard => s     -- set by each machine below
  -- `listen`: `self._call_say = say`
  | .callStart => { s with listener := s.next, next := s.next + 1 }
  -- `listen`, the holder's own end: `self._call_say = None`
  | .callEnd => { s with listener := 0 }
  -- `_spawn` → `asyncio.ensure_future(coro)`: the new task runs in a copy of the spawner's context.
  | .spawn j => match s.jobs[j]? with
      | some job => { s with jobs := s.jobs ++ [job] }
      | none => s
  | .reply _ => s   -- set by each machine below
  -- `_handle` → `_spawn(_type_to_claude(…))`: the session's next answer answers this job's words. (Fix:
  -- telegram.py:3191 `self._relay_from_call = _FROM_CALL.get()`, which is `prompt.began`.)
  | .forward j => match s.jobs[j]? with
      | some job => { s with prompt := job, queue := s.queue ++ [job] }
      | none => s
  | .relay => s     -- set by each machine below

/-- A line said by `job`, read out on the call (`read`) or not. -/
def say (s : S) (job : Job) (read : Bool) : S :=
  { s with readBad := s.readBad || (read && !job.spoken),
           missed := s.missed || (job.spoken && job.call != 0 && s.listener == job.call && !read),
           crossCall := s.crossCall || (read && s.listener != job.call) }

/-! ## The old code (48df362) -/

def Old.step (s : S) : Event → S
  -- old `hear`: `_hear`'s body ran with no mark.
  | .heard => { s with jobs := s.jobs ++ [{ flag := false, spoken := true, call := s.listener }] }
  -- old `_say` (telegram.py:2449) and `_to_call` (4143): `self._call_say is not None`, whoever's work it is.
  | .reply j => match s.jobs[j]? with
      | some job => say s job (s.listener != 0)
      | none => s
  -- old relay lines went through the same `_say` / `_send_choices` / `_to_call`: read whenever a call is on.
  | .relay => say s s.prompt (s.listener != 0)
  | e => common s e

def Old.run (evs : List Event) : S := evs.foldl Old.step {}

/-- A call is open, the owner types a message in Telegram, and its answer is said: it is read aloud. -/
def typedTrace : List Event := [.callStart, .typed, .spawn 0, .reply 1]

/-- The same through Claude: a prompt typed in Telegram during a call, and Claude's answer is read aloud. -/
def typedClaudeTrace : List Event := [.callStart, .typed, .forward 0, .relay]

theorem old_violates :
    let s := Old.run typedTrace
    s.listener ≠ 0 ∧ (s.jobs[1]?.map Job.spoken) = some false ∧ s.readBad = true ∧ Spec s = false ∧
    (Old.run typedClaudeTrace).readBad = true := by
  decide

/-! ## The code at 2ad3550 -/

def Cur.step (s : S) : Event → S
  -- telegram.py:4126-4130 `hear`: `_FROM_CALL.set(True)` around `_hear`.
  | .heard => { s with jobs := s.jobs ++ [{ flag := true, spoken := true, call := s.listener }] }
  -- telegram.py:4099-4101 `_call_reader`: `self._call_say if _FROM_CALL.get() else None`; `_say` (2478),
  -- `_send_choices` (2546), `_think` (3580, 3593-3595, 3607-3611) and `_to_call` (4206-4208) read only through it.
  | .reply j => match s.jobs[j]? with
      | some job => say s job (job.flag && s.listener != 0)
      | none => s
  -- daemon.py:3048/2555 `asyncio.create_task(…)`, and the hook handlers that await `decide_permission`: the line is
  -- said from a daemon task, whose context never had `_FROM_CALL` set, whoever typed the prompt.
  | .relay =>
      let daemonFlag := false
      say s s.prompt (daemonFlag && s.listener != 0)
  | e => common s e

def Cur.run (evs : List Event) : S := evs.foldl Cur.step {}

/-! ### P1 at 2ad3550, over the core events -/

/-- Every job's flag is its origin, and no line was read for work not begun on a call. -/
def Inv (s : S) : Prop := (∀ job ∈ s.jobs, job.flag = job.spoken) ∧ s.readBad = false

theorem inv_init : Inv ({} : S) := by
  refine ⟨?_, rfl⟩
  intro job h
  simp at h

/-- A line said by a job whose flag is its origin, read through 2ad3550's `_call_reader`, keeps P1 and P2. -/
theorem say_ok (s : S) (k : Job) (hk : k.flag = k.spoken) (hb : s.readBad = false) (hm : s.missed = false) :
    (say s k (k.flag && s.listener != 0)).readBad = false ∧
    (say s k (k.flag && s.listener != 0)).missed = false := by
  rw [hk]
  cases hs : k.spoken with
  | false => simp [say, hb, hm, hs]
  | true =>
      refine ⟨by simp [say, hb, hs], ?_⟩
      by_cases hc : k.call = 0 <;> by_cases hl : s.listener = k.call <;> simp [say, hm, hs, hc, hl]

theorem inv_step (s : S) (e : Event) (h : Inv s) : Inv (Cur.step s e) := by
  obtain ⟨hj, hb⟩ := h
  cases e with
  | typed =>
      refine ⟨?_, hb⟩
      intro job hmem
      simp only [Cur.step, common, List.mem_append, List.mem_singleton] at hmem
      rcases hmem with hmem | rfl
      · exact hj job hmem
      · rfl
  | heard =>
      refine ⟨?_, hb⟩
      intro job hmem
      simp only [Cur.step, List.mem_append, List.mem_singleton] at hmem
      rcases hmem with hmem | rfl
      · exact hj job hmem
      · rfl
  | callStart => exact ⟨hj, hb⟩
  | callEnd => exact ⟨hj, hb⟩
  | spawn j =>
      simp only [Cur.step, common]
      cases hg : s.jobs[j]? with
      | none => exact ⟨hj, hb⟩
      | some k =>
          refine ⟨?_, hb⟩
          intro job hmem
          simp only [List.mem_append, List.mem_singleton] at hmem
          rcases hmem with hmem | rfl
          · exact hj job hmem
          · exact hj job (List.mem_of_getElem? hg)
  | reply j =>
      simp only [Cur.step]
      cases hg : s.jobs[j]? with
      | none => exact ⟨hj, hb⟩
      | some k =>
          have hk : k.flag = k.spoken := hj k (List.mem_of_getElem? hg)
          refine ⟨hj, ?_⟩
          simp only [say, hb, hk]
          cases k.spoken <;> simp
  | forward j =>
      simp only [Cur.step, common]
      cases hg : s.jobs[j]? with
      | none => exact ⟨hj, hb⟩
      | some k => exact ⟨hj, hb⟩
  | relay =>
      refine ⟨hj, ?_⟩
      simp [Cur.step, say, hb]

theorem inv_run (evs : List Event) : ∀ s : S, Inv s → Inv (evs.foldl Cur.step s) := by
  induction evs with
  | nil => intro s h; exact h
  | cons e rest ih => intro s h; exact ih _ (inv_step s e h)

/-- At 2ad3550, for every trace of typed messages, heard words, calls starting and ending, tasks spawned, lines
said, prompts typed into the Claude session and lines relayed from it: P1 holds. (The chat window and the "claude
on" queue, below, are where it did not.) -/
theorem current_p1_invariant (evs : List Event) : P1 (Cur.run evs) = true := by
  obtain ⟨_, hb⟩ := inv_run evs {} inv_init
  unfold P1 Cur.run
  rw [hb]
  rfl

/-- The old counterexample at 2ad3550: the typed message's answer stays in the chat, and so does Claude's answer
to a typed prompt; words heard on the call are answered on it. -/
theorem current_replays_old_counterexample :
    (Cur.run typedTrace).readBad = false ∧
    (Cur.run typedClaudeTrace).readBad = false ∧
    (Cur.run [.callStart, .heard, .spawn 0, .reply 1]).crossCall = false ∧
    (Cur.run [.callStart, .heard, .spawn 0, .reply 1]).missed = false ∧
    (Old.run [.callStart, .heard, .spawn 0, .reply 1]).readBad = false := by
  decide

theorem missed_step (s : S) (e : Event) (h : Inv s) (hm : s.missed = false) (he : e ≠ .relay) :
    (Cur.step s e).missed = false := by
  obtain ⟨hj, hb⟩ := h
  cases e with
  | typed => exact hm
  | heard => exact hm
  | callStart => exact hm
  | callEnd => exact hm
  | spawn j =>
      simp only [Cur.step, common]
      cases s.jobs[j]? <;> exact hm
  | reply j =>
      simp only [Cur.step]
      cases hg : s.jobs[j]? with
      | none => exact hm
      | some k => exact (say_ok s k (hj k (List.mem_of_getElem? hg)) hb hm).2
  | forward j =>
      simp only [Cur.step, common]
      cases s.jobs[j]? <;> exact hm
  | relay => exact absurd rfl he

/-- At 2ad3550, P2 holds for every trace in which no line comes from the Claude relay. -/
theorem current_p2_without_relay (evs : List Event) (h : ∀ e ∈ evs, e ≠ .relay) : P2 (Cur.run evs) = true := by
  have key : ∀ (evs : List Event) (s : S), (∀ e ∈ evs, e ≠ .relay) → Inv s → s.missed = false →
      (evs.foldl Cur.step s).missed = false := by
    intro evs
    induction evs with
    | nil => intro s _ _ hm; exact hm
    | cons e rest ih =>
        intro s hr hi hm
        have he : e ≠ .relay := hr e (List.mem_cons_self ..)
        exact ih _ (fun x hx => hr x (List.mem_cons_of_mem _ hx)) (inv_step s e hi) (missed_step s e hi hm he)
  unfold P2 Cur.run
  rw [key evs {} h inv_init rfl]
  rfl

/-! ### The findings at 2ad3550 -/

/-- Every way words reach a job: the core events, the Mac chat window, and a message held by "claude on". -/
inductive Ev where
  | core (e : Event)
  | window                   -- daemon.py `_ipc_chat`: words typed in the Mac chat window go to `tg.hear`
  | held (spoken : Bool) (j : Nat)   -- a message held while job j's "claude on" lists, routed once it has
  -- (added for the findings at 1dd0f26)
  | heardOn (c : Nat)        -- call c (open, holding the listener or not) hears the owner's words: Call.heard
  | turnBegin                -- Claude begins the oldest prompt eve typed (its UserPromptSubmit hook)
  | turnBeginMac             -- Claude begins a prompt typed at the Mac's keyboard (its hook; not eve's)
  | line                     -- one line from the Claude session arrives (`relay_line`, from any of its paths)
  | flush                    -- `_flush_relay` sends the waiting lines
  deriving DecidableEq, Repr

/-- The ghost events of Claude's turns, the same in every machine: Claude Code begins its queued prompts in order. -/
def turns (s : S) : Ev → S
  | .turnBegin => match s.queue with
      | j :: rest => { s with answering := j, queue := rest }
      | [] => s
  | .turnBeginMac => { s with answering := { flag := false, spoken := false, call := 0 } }
  | _ => s

def Cur.gap (s : S) : Ev → S
  | .core e => Cur.step s e
  -- daemon.py:2491 `tg.hear(text)`: `hear` sets `_FROM_CALL` for typed words.
  | .window => { s with jobs := s.jobs ++ [{ flag := true, spoken := false, call := 0 }] }
  -- telegram.py:2161-2163 holds it, 3080-3082 routes it in j's task: j's flag, whatever its own origin.
  | .held spoken j => match s.jobs[j]? with
      | some job => { s with jobs := s.jobs ++
                       [{ flag := job.flag, spoken := spoken, call := if spoken then s.listener else 0 }] }
      | none => s
  -- `hear` at 2ad3550 set True whichever call heard the words.
  | .heardOn c => if c != 0 && c < s.next then
        { s with jobs := s.jobs ++ [{ flag := true, spoken := true, call := c }] } else s
  -- 2ad3550 is modelled with `.core .relay` alone: its relay read nothing, batched or not.
  | e => turns s e

def Cur.gapRun (evs : List Ev) : S := evs.foldl Cur.gap {}

/-- (1) Words heard on a call are typed into the Claude session, and Claude's answer reaches the chat while that
call still holds the listener. -/
def relayTrace : List Event := [.callStart, .heard, .forward 0, .relay]

/-- (2) Words on call 1 start a task, call 1 ends, call 2 starts, and the task's result is said. -/
def crossTrace : List Event := [.callStart, .heard, .spawn 0, .callEnd, .callStart, .reply 1]

/-- (3) A call is open, and words typed in the Mac chat window are answered. -/
def windowTrace : List Ev := [.core .callStart, .window, .core (.reply 0)]

/-- (4a) "claude on" said on a call; a Telegram message typed while it lists is answered. -/
def heldTypedTrace : List Ev := [.core .callStart, .core .heard, .held false 0, .core (.reply 1)]

/-- (4b) "claude on" typed during a call; words said on the call while it lists are answered. -/
def heldSpokenTrace : List Ev := [.core .callStart, .core .typed, .held true 0, .core (.reply 1)]

/-- (1) at 2ad3550: Claude's answer to the spoken prompt is not read (P2 fails); the old code read it. -/
theorem current_violates :
    (Cur.run relayTrace).listener = (Cur.run relayTrace).prompt.call ∧
    (Cur.run relayTrace).prompt.spoken = true ∧
    (Cur.run relayTrace).missed = true ∧ Spec (Cur.run relayTrace) = false ∧
    (Old.run relayTrace).missed = false := by
  decide

/-- (1) under the name the replay test cites (bridge/tests/test_telegram.py). -/
theorem current_relay_unread : (Cur.run relayTrace).missed = true ∧ (Old.run relayTrace).missed = false :=
  ⟨current_violates.2.2.1, current_violates.2.2.2.2⟩

/-- (2)-(4) at 2ad3550: call 1's work is read on call 2 (P1 and P2 still hold, P3 does not); the chat window's
typed words are read (P1); a held typed message is read (P1); held spoken words are not (P2). -/
theorem current_gaps :
    (Cur.run crossTrace).crossCall = true ∧ Spec (Cur.run crossTrace) = true ∧
    (Cur.gapRun windowTrace).readBad = true ∧
    (Cur.gapRun heldTypedTrace).readBad = true ∧
    (Cur.gapRun heldSpokenTrace).missed = true := by
  decide

/-! ## The code at 1dd0f26 (the first fix)

Its lines are those of the files at 1dd0f26: the machine the first fix was proved on, where `heard` meant "the
call holding the listener heard the words" and a relay line was attributed to the last prompt typed. With the call
that hears the words and the prompt Claude answers as ghosts, it breaks P1-P3 (`current_hearing_call`,
`current_relay_fifo`). The `flag` field is 2ad3550's Bool and plays no part here; `_FROM_CALL` is `began`. -/

/-- `_flush_relay` at 1dd0f26 (telegram.py:3246): every waiting line, read with `_relay_from_call` at the flush. -/
def Mid.flushLines (s : S) : List (Job × Nat) → S
  | [] => s
  | (j, _) :: rest => Mid.flushLines (say s j (s.prompt.began != 0 && s.listener == s.prompt.began)) rest

def Mid.step (s : S) : Ev → S
  -- telegram.py:4183 `hear`: `_FROM_CALL.set(self._call_say if on_call else None)`, the call holding the listener
  -- (none when no call does).
  | .core .heard => { s with jobs := s.jobs ++
                        [{ flag := false, spoken := true, call := s.listener, began := s.listener }] }
  -- telegram.py:4151-4156 `_call_reader`: `say if began_on is not None and say is not None and say == began_on`;
  -- `_say` (2507), `_send_choices` (2575), `_think` (3630, and each streamed sentence through `_to_call`, 3647)
  -- and `_to_call` (4263) read only through it.
  | .core (.reply j) => match s.jobs[j]? with
      | some job => say s job (job.began != 0 && s.listener == job.began)
      | none => s
  -- telegram.py:3246 `_flush_relay`, 3331 `relay_notification`, 3371 `decide_permission`:
  -- `_FROM_CALL.set(self._relay_from_call)`, the call the LAST prompt typed was said on (set at 3191), whichever
  -- prompt Claude is answering.
  | .core .relay => say s s.answering (s.prompt.began != 0 && s.listener == s.prompt.began)
  | .core e => common s e
  -- daemon.py:2492 `tg.hear(text, on_call=False)`: typed, so `_FROM_CALL` is None.
  | .window => { s with jobs := s.jobs ++ [{ flag := false, spoken := false, call := 0, began := 0 }] }
  -- telegram.py:2191 held with its own `_FROM_CALL.get()`; 3109-3117 replayed under it, not under job j's.
  | .held spoken _ =>
      let origin := if spoken then s.listener else 0
      { s with jobs := s.jobs ++ [{ flag := false, spoken := spoken, call := origin, began := origin }] }
  -- phone_call.py `Call.heard` → `brain.hear(text)`: `hear` marks the work with the LISTENER's reader, whichever
  -- call heard it. A desk call's `listening` stays True after a phone call took the listener (phone_call.py
  -- `_take_chat`), so the desk's words were marked as the phone's.
  | .heardOn c => if c != 0 && c < s.next then
        { s with jobs := s.jobs ++ [{ flag := false, spoken := true, call := c, began := s.listener }] } else s
  -- telegram.py:3238 `relay_line` keeps no origin.
  | .line => { s with batch := s.batch ++ [(s.answering, 0)] }
  | .flush => { (Mid.flushLines s s.batch) with batch := [] }
  | e => turns s e

def Mid.run (evs : List Ev) : S := evs.foldl Mid.step {}

/-- (6) The hearing call: a desk call is on (call 1), a phone call takes the listener (call 2), and the desk hears
the owner's words. Their answer is read on the phone. -/
def hearingTrace : List Ev :=
  [.core .callStart, .core .callStart, .heardOn 1, .core (.spawn 0), .core (.reply 1)]

/-- (7a) A typed prompt is running; a spoken prompt is queued behind it; the typed prompt's answer arrives. -/
def fifoTypedFirst : List Ev :=
  [.core .callStart, .core .typed, .core (.forward 0), .turnBegin, .core .heard, .core (.forward 1), .core .relay]

/-- (7b) The reverse: the spoken prompt is running, a typed one is queued, the spoken prompt's answer arrives. -/
def fifoSpokenFirst : List Ev :=
  [.core .callStart, .core .heard, .core (.forward 0), .turnBegin, .core .typed, .core (.forward 1), .core .relay]

/-- (7c) One batch straddles two prompts: the spoken prompt's line, then the typed prompt's, then the flush. -/
def fifoStraddle : List Ev :=
  [.core .callStart, .core .heard, .core (.forward 0), .turnBegin, .line, .core .typed, .core (.forward 1),
   .turnBegin, .line, .flush]

/-- (7d) A spoken prompt went in, and the owner typed a prompt at the Mac's keyboard, which Claude begins. -/
def fifoMacTurn : List Ev :=
  [.core .callStart, .core .heard, .core (.forward 0), .turnBeginMac, .core .relay]

/-- (6) at 1dd0f26: the desk's words are answered on the phone (P3). -/
theorem current_hearing_call :
    (Mid.run hearingTrace).crossCall = true ∧ (Mid.run hearingTrace).listener = 2 ∧
    (Mid.run hearingTrace).jobs[0]?.map Job.call = some 1 := by
  decide

/-- (7) at 1dd0f26: a relay line is read with the last prompt's origin. A typed prompt's answer is read aloud
(P1), a spoken prompt's answer is lost (P2), a batch is lost whole (P2), and a Mac turn's answer is read (P1). -/
theorem current_relay_fifo :
    (Mid.run fifoTypedFirst).readBad = true ∧
    (Mid.run fifoSpokenFirst).missed = true ∧
    (Mid.run fifoStraddle).missed = true ∧
    (Mid.run fifoMacTurn).readBad = true := by
  decide

/-! ## The fixed code (fix/lean-findings, after 1dd0f26)

Its lines are those of the fixed files:
- phone_call.py:702-706 `Call.heard` passes `call=self.say` to a brain whose `hear` takes it (`_takes_call`, 480);
  telegram.py:4224-4241 `hear(…, call=…)`: the hearing call takes the listener back if it no longer holds it
  (4239), and the work is marked with that reader (4241). The desk call is a `Call` too (desk_call.py `_start`).
- the relay FIFO: telegram.py:3203-3204 `_type_to_claude` queues (the prompt's words, its `_FROM_CALL`) on
  `_relay_prompts` before typing (3212: removed if it never went in); daemon.py:2412 the UserPromptSubmit hook
  calls telegram.py:3342-3359 `relay_turn_began`, which pops the prompt with these words (and any older ones that
  never began) and makes its origin `_relay_origin` (3357), or None for a prompt eve did not type (3359);
  `relay_line` keeps each line's origin as it arrives (3263); `_flush_relay` groups by origin (3277) and reads each
  message with its own (3287); `relay_notification` (3377) and `decide_permission` (3406, 3418) take
  `_relay_origin` when they arrive.

Abstractions, stated: the hook's prompt text identifies the prompt eve typed (`_prompt_key`: whitespace runs
as one space), so `relay_turn_began` pops the queue's head for `turnBegin` and finds nothing for `turnBeginMac`
(a Mac-typed prompt is not word for word one eve has queued). Claude Code begins its queued prompts in order.
-/

/-- `_flush_relay` (telegram.py:3267-3287): each waiting line, read with the origin it arrived with. -/
def Fix.flushLines (s : S) : List (Job × Nat) → S
  | [] => s
  | (j, o) :: rest => Fix.flushLines (say s j (o != 0 && s.listener == o)) rest

def Fix.step (s : S) : Ev → S
  | .core .heard => { s with jobs := s.jobs ++
                        [{ flag := false, spoken := true, call := s.listener, began := s.listener }] }
  | .core (.reply j) => match s.jobs[j]? with
      | some job => say s job (job.began != 0 && s.listener == job.began)
      | none => s
  -- `relay_notification` (3377), `decide_permission` (3406): the turn's origin as the line arrives.
  | .core .relay => say s s.answering (s.answering.began != 0 && s.listener == s.answering.began)
  | .core e => common s e
  | .window => { s with jobs := s.jobs ++ [{ flag := false, spoken := false, call := 0, began := 0 }] }
  | .held spoken _ =>
      let origin := if spoken then s.listener else 0
      { s with jobs := s.jobs ++ [{ flag := false, spoken := spoken, call := origin, began := origin }] }
  -- telegram.py:4236-4241: the hearing call takes the listener back and marks the work with its own reader.
  | .heardOn c => if c != 0 && c < s.next then
        { s with listener := c, jobs := s.jobs ++ [{ flag := false, spoken := true, call := c, began := c }] }
      else s
  -- telegram.py:3263: the origin of the turn Claude is in, as the line arrives.
  | .line => { s with batch := s.batch ++ [(s.answering, s.answering.began)] }
  | .flush => { s with readBad := (Fix.flushLines s s.batch).readBad, missed := (Fix.flushLines s s.batch).missed,
                       crossCall := (Fix.flushLines s s.batch).crossCall, batch := [] }
  | e => turns s e

def Fix.run (evs : List Ev) : S := evs.foldl Fix.step {}

/-- A job whose `_FROM_CALL` is the call that heard it, and which no call heard unless it was spoken. -/
def Good (k : Job) : Prop := k.began = k.call ∧ (k.spoken = false → k.call = 0)

def FInv (s : S) : Prop :=
  (∀ k ∈ s.jobs, Good k) ∧ Good s.prompt ∧ (∀ k ∈ s.queue, Good k) ∧ Good s.answering ∧
  (∀ p ∈ s.batch, Good p.1 ∧ p.2 = p.1.began) ∧
  s.readBad = false ∧ s.missed = false ∧ s.crossCall = false

/-- A line said by a good job, read through the fixed `_call_reader`, keeps P1, P2 and P3. -/
theorem fixed_say_ok (s : S) (k : Job) (hk : Good k) (hb : s.readBad = false) (hm : s.missed = false)
    (hx : s.crossCall = false) :
    let t := say s k (k.began != 0 && s.listener == k.began)
    t.readBad = false ∧ t.missed = false ∧ t.crossCall = false := by
  obtain ⟨he, hs⟩ := hk
  rw [he]
  cases hsp : k.spoken with
  | false =>
      have hc := hs hsp
      simp [say, hb, hm, hx, hsp, hc]
  | true =>
      by_cases hc : k.call = 0 <;> by_cases hl : s.listener = k.call <;> simp [say, hb, hm, hx, hsp, hc, hl]

/-- A flush of good lines, each read with its own origin, keeps P1, P2 and P3. -/
theorem fixed_flush_ok : ∀ (l : List (Job × Nat)) (s : S), (∀ p ∈ l, Good p.1 ∧ p.2 = p.1.began) →
    s.readBad = false → s.missed = false → s.crossCall = false →
    (Fix.flushLines s l).readBad = false ∧ (Fix.flushLines s l).missed = false ∧
      (Fix.flushLines s l).crossCall = false := by
  intro l
  induction l with
  | nil => intro s _ hb hm hx; exact ⟨hb, hm, hx⟩
  | cons p rest ih =>
      intro s hl hb hm hx
      obtain ⟨j, o⟩ := p
      have hp := hl (j, o) (List.mem_cons_self ..)
      obtain ⟨hg, ho⟩ := hp
      simp only at ho
      have := fixed_say_ok s j hg hb hm hx
      simp only [Fix.flushLines]
      rw [ho]
      exact ih _ (fun q hq => hl q (List.mem_cons_of_mem _ hq)) this.1 this.2.1 this.2.2

theorem finv_step (s : S) (e : Ev) (h : FInv s) : FInv (Fix.step s e) := by
  obtain ⟨hj, hp, hq, ha, hbt, hb, hm, hx⟩ := h
  have push : ∀ (k : Job), Good k → ∀ k' ∈ s.jobs ++ [k], Good k' := by
    intro k hk k' hmem
    simp only [List.mem_append, List.mem_singleton] at hmem
    rcases hmem with hmem | rfl
    · exact hj k' hmem
    · exact hk
  cases e with
  | window => exact ⟨push _ ⟨rfl, fun _ => rfl⟩, hp, hq, ha, hbt, hb, hm, hx⟩
  | held spoken j =>
      refine ⟨push _ ⟨rfl, ?_⟩, hp, hq, ha, hbt, hb, hm, hx⟩
      intro h0
      simp only at h0
      simp [h0]
  | heardOn c =>
      simp only [Fix.step]
      split
      · exact ⟨push _ ⟨rfl, fun h0 => Bool.noConfusion h0⟩, hp, hq, ha, hbt, hb, hm, hx⟩
      · exact ⟨hj, hp, hq, ha, hbt, hb, hm, hx⟩
  | turnBegin =>
      change FInv (turns s .turnBegin)
      unfold turns
      cases hqe : s.queue with
      | nil => exact ⟨hj, hp, hq, ha, hbt, hb, hm, hx⟩
      | cons j rest =>
          refine ⟨hj, hp, fun k hk => hq k (by rw [hqe]; exact List.mem_cons_of_mem _ hk),
            hq j (by rw [hqe]; exact List.mem_cons_self ..), hbt, hb, hm, hx⟩
  | turnBeginMac =>
      exact ⟨hj, hp, hq, ⟨rfl, fun _ => rfl⟩, hbt, hb, hm, hx⟩
  | line =>
      refine ⟨hj, hp, hq, ha, ?_, hb, hm, hx⟩
      intro p hmem
      change p ∈ s.batch ++ [(s.answering, s.answering.began)] at hmem
      simp only [List.mem_append, List.mem_singleton] at hmem
      rcases hmem with hmem | rfl
      · exact hbt p hmem
      · exact ⟨ha, rfl⟩
  | flush =>
      have := fixed_flush_ok s.batch s hbt hb hm hx
      refine ⟨hj, hp, hq, ha, fun p hp' => ?_, this.1, this.2.1, this.2.2⟩
      change p ∈ ([] : List (Job × Nat)) at hp'
      simp at hp'
  | core e =>
      cases e with
      | typed => exact ⟨push _ ⟨rfl, fun _ => rfl⟩, hp, hq, ha, hbt, hb, hm, hx⟩
      | heard => exact ⟨push _ ⟨rfl, fun h0 => Bool.noConfusion h0⟩, hp, hq, ha, hbt, hb, hm, hx⟩
      | callStart => exact ⟨hj, hp, hq, ha, hbt, hb, hm, hx⟩
      | callEnd => exact ⟨hj, hp, hq, ha, hbt, hb, hm, hx⟩
      | spawn j =>
          simp only [Fix.step, common]
          cases hg : s.jobs[j]? with
          | none => exact ⟨hj, hp, hq, ha, hbt, hb, hm, hx⟩
          | some k => exact ⟨push k (hj k (List.mem_of_getElem? hg)), hp, hq, ha, hbt, hb, hm, hx⟩
      | forward j =>
          simp only [Fix.step, common]
          cases hg : s.jobs[j]? with
          | none => exact ⟨hj, hp, hq, ha, hbt, hb, hm, hx⟩
          | some k =>
              have hk := hj k (List.mem_of_getElem? hg)
              refine ⟨hj, hk, ?_, ha, hbt, hb, hm, hx⟩
              intro k' hmem
              simp only [List.mem_append, List.mem_singleton] at hmem
              rcases hmem with hmem | rfl
              · exact hq k' hmem
              · exact hk
      | reply j =>
          simp only [Fix.step]
          cases hg : s.jobs[j]? with
          | none => exact ⟨hj, hp, hq, ha, hbt, hb, hm, hx⟩
          | some k =>
              have := fixed_say_ok s k (hj k (List.mem_of_getElem? hg)) hb hm hx
              exact ⟨hj, hp, hq, ha, hbt, this.1, this.2.1, this.2.2⟩
      | relay =>
          have := fixed_say_ok s s.answering ha hb hm hx
          exact ⟨hj, hp, hq, ha, hbt, this.1, this.2.1, this.2.2⟩

/-- The fixed code, for every trace of typed messages, heard words (on the call holding the listener or on
another), words typed in the Mac chat window, messages held by "claude on", calls starting and ending, tasks
spawned, lines said, prompts typed into the Claude session, Claude's turns (eve's prompts and the Mac's), and
relay lines arriving and flushed in batches: a line is read only for work begun on a call (P1), call work is read
while its call holds the listener, the Claude relay included (P2), and only on that call (P3). -/
theorem fixed_invariant (evs : List Ev) :
    Spec (Fix.run evs) = true ∧ P3 (Fix.run evs) = true := by
  have key : ∀ (evs : List Ev) (s : S), FInv s → FInv (evs.foldl Fix.step s) := by
    intro evs
    induction evs with
    | nil => intro s h; exact h
    | cons e rest ih => intro s h; exact ih _ (finv_step s e h)
  have hi : FInv (Fix.run evs) :=
    key evs {} ⟨by intro k h; simp at h, ⟨rfl, fun _ => rfl⟩, by intro k h; simp at h, ⟨rfl, fun _ => rfl⟩,
      by intro p h; simp at h, rfl, rfl, rfl⟩
  obtain ⟨_, _, _, _, _, hb, hm, hx⟩ := hi
  unfold Spec P1 P2 P3
  rw [hb, hm, hx]
  exact ⟨rfl, rfl⟩

/-- Every counterexample, on the fixed code: each is answered right, and words spoken on the call still are. The
relay's prompts are begun by Claude (`turnBegin`) before their answers come. -/
theorem fixed_replays :
    (Fix.run ((relayTrace.take 3).map .core ++ [.turnBegin, .core .relay])).missed = false ∧
    (Fix.run ((typedClaudeTrace.take 3).map .core ++ [.turnBegin, .core .relay])).readBad = false ∧
    (Fix.run (typedTrace.map .core)).readBad = false ∧
    (Fix.run (crossTrace.map .core)).crossCall = false ∧
    (Fix.run windowTrace).readBad = false ∧
    (Fix.run heldTypedTrace).readBad = false ∧
    (Fix.run heldSpokenTrace).missed = false ∧
    (Fix.run [.core .callStart, .core .heard, .core (.spawn 0), .core (.reply 1)]).missed = false ∧
    (Fix.run hearingTrace).crossCall = false ∧ (Fix.run hearingTrace).listener = 1 ∧
    (Fix.run (hearingTrace ++ [.heardOn 2, .core (.spawn 2), .core (.reply 3)])).crossCall = false ∧
    (Fix.run fifoTypedFirst).readBad = false ∧
    (Fix.run (fifoTypedFirst ++ [.turnBegin, .core .relay])).missed = false ∧
    (Fix.run fifoSpokenFirst).missed = false ∧
    (Fix.run fifoStraddle).missed = false ∧ (Fix.run fifoStraddle).readBad = false ∧
    (Fix.run fifoMacTurn).readBad = false := by
  decide

end Eve.CallScope
