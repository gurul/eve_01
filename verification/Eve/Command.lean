/-
# The Bash allow tier (bridge/src/eve_bridge/matchers.py)

`classify_command` answers a Bash command the daemon relays with "allow",
"ask" or "default". "allow" is the strong one: the daemon returns an explicit
allow at once (daemon.py `_handle_pretooluse`), before Jev or the phone sees
the command, and it overrides Claude Code's own prompt.

The property: **"allow" only ever answers one simple command whose program
cannot run another command or write a file** — for every matcher config the
owner can write, not just the built-in lists.

`current_violates` shows the code before the fix broke it: the built-in
pattern `^echo( |$)` is anchored only at the start, so `echo x; rm -rf ~` was
allowed. `fixed_invariant` proves the guarded classifier upholds it for every
config and every command.

Trusted assumptions (stated, not proved):
* The shell reads the characters in `shellMeta`, and whitespace other than a
  space, as structure; a command without them is words separated by spaces,
  and its first word is the program that runs (`toy_shell_one_command` proves
  this for the toy shell below, whose fidelity to zsh/bash is the assumption).
* The runner list and the run-or-write flag table cover the programs the
  owner allows. The theorem is exactly as strong as that table.
* Python's `str.isspace` rejects a superset of `isWs`, so Python's
  `is_simple` true implies the model's `simple` true.
-/

namespace Eve.Command

abbrev Cmd := List Char

inductive Decision | allow | ask | default
  deriving DecidableEq, Repr

/-- A matcher config: any predicates at all stand in for the owner's regexes. -/
structure Config where
  allow  : List (Cmd → Bool)
  ask    : List (Cmd → Bool)
  strict : Bool

/-! ## The classifier before the fix — matchers.py `classify_command` -/

def fallthrough (cfg : Config) : Decision := if cfg.strict then .ask else .default

def classifyCurrent (cfg : Config) (c : Cmd) : Decision :=
  if c.isEmpty then fallthrough cfg                  -- `if not command`
  else if cfg.ask.any (· c) then .ask                -- always_ask first
  else if cfg.allow.any (· c) then .allow            -- then auto_allow
  else fallthrough cfg

/-! ## `is_simple` — matchers.py, mirrored line for line -/

/-- `_SHELL_META` -/
def shellMeta : List Char := [';', '&', '|', '<', '>', '$', '`', '(', ')', '{', '}', '\\', '\'', '"', '#', '!']
/-- `_GLOB` -/
def globChars : List Char := ['*', '?', '[']
/-- The whitespace the model knows; Python's `isspace` is a superset. -/
def isWs (ch : Char) : Bool := ch == ' ' || ch == '\t' || ch == '\n' || ch == '\r'

def runners : List Cmd :=
  ["env", "xargs", "nice", "nohup", "timeout", "time", "command", "exec",
   "eval", "watch", "sh", "bash", "zsh", "sudo", "doas", "su"].map String.toList

/-- `_RUN_OR_WRITE_FLAGS`: long-flag prefixes and short-cluster letters. -/
def runOrWrite (prog : Cmd) : Option (List Cmd × List Char) :=
  if prog == "find".toList then some (["-exec", "-ok", "-delete", "-fprint", "-fls"].map String.toList, [])
  else if prog == "fd".toList then some (["--exec"].map String.toList, ['x', 'X'])
  else if prog == "rg".toList then some (["--pre"].map String.toList, [])
  else if prog == "tree".toList then some ([], ['o'])
  else if prog == "git".toList then some (["--output", "--ext-diff", "-c"].map String.toList, [])
  else none

/-- `command.split(" ")` -/
def splitSp : Cmd → Cmd → List Cmd
  | [], acc => [acc.reverse]
  | ch :: t, acc => if ch == ' ' then acc.reverse :: splitSp t [] else splitSp t (ch :: acc)

/-- `[w for w in command.split(" ") if w]` -/
def words (c : Cmd) : List Cmd := (splitSp c []).filter (fun w => !w.isEmpty)

def badChar (ch : Char) : Bool := shellMeta.contains ch || (isWs ch && ch != ' ')

/-- A short-flag cluster: `-Hx`, not `--exec`. -/
def shortCluster (w : Cmd) : Bool := w.head? == some '-' && w.take 2 != ['-', '-']

def argRuns (prefixes : List Cmd) (letters : List Char) (w : Cmd) : Bool :=
  prefixes.any (fun p => p.isPrefixOf w) || (shortCluster w && letters.any (fun l => w.contains l))

def simple (c : Cmd) : Bool :=
  if c.any badChar then false
  else match words c with
    | [] => false
    | prog :: args =>
      if prog.contains '=' then false
      else if runners.contains prog then args.isEmpty
      else match runOrWrite prog with
        | none => true
        | some (prefixes, letters) =>
          if c.any (fun ch => globChars.contains ch) then false
          else !(args.any (argRuns prefixes letters))

/-! ## The classifier after the fix -/

def classifyFixed (cfg : Config) (c : Cmd) : Decision :=
  if c.isEmpty then fallthrough cfg
  else if cfg.ask.any (· c) then .ask
  else if simple c && cfg.allow.any (· c) then .allow   -- `if is_simple(command)`
  else fallthrough cfg

/-! ## The specification, stated apart from the checker -/

/-- No shell structure: no meta character, no whitespace but the space. -/
def NoStructure (c : Cmd) : Prop := ∀ ch ∈ c, ch ∉ shellMeta ∧ (isWs ch = true → ch = ' ')

/-- The program cannot run another command or write a file. -/
def RunsOnlyItself (prog : Cmd) (args : List Cmd) (c : Cmd) : Prop :=
  '=' ∉ prog ∧
  (prog ∈ runners → args = []) ∧
  ∀ prefixes letters, runOrWrite prog = some (prefixes, letters) →
    (∀ ch ∈ c, ch ∉ globChars) ∧
    ∀ w ∈ args, (∀ p ∈ prefixes, ¬ p <+: w) ∧ (shortCluster w = true → ∀ l ∈ letters, l ∉ w)

def Simple (c : Cmd) : Prop :=
  NoStructure c ∧ ∃ prog args, words c = prog :: args ∧ RunsOnlyItself prog args c

/-! ## A toy shell: what "one simple command" means -/

/-- The shell's command separators. -/
def separators : List Char := [';', '&', '|', '\n']

/-- Split a command line into the commands the shell runs. -/
def segments : Cmd → Cmd → List Cmd
  | [], acc => [acc.reverse]
  | ch :: t, acc => if separators.contains ch then acc.reverse :: segments t [] else segments t (ch :: acc)

theorem segments_no_sep (c acc : Cmd) (h : ∀ ch ∈ c, ch ∉ separators) :
    segments c acc = [acc.reverse ++ c] := by
  induction c generalizing acc with
  | nil => simp [segments]
  | cons ch t ih =>
    have hch : ch ∉ separators := h ch (by simp)
    have ht : ∀ x ∈ t, x ∉ separators := fun x hx => h x (by simp [hx])
    simp [segments, ih _ ht, hch]

/-- Without shell structure the toy shell runs exactly one command, with no
substitution and no redirection. -/
theorem toy_shell_one_command (c : Cmd) (h : NoStructure c) :
    segments c [] = [c] ∧ '$' ∉ c ∧ '`' ∉ c ∧ '>' ∉ c ∧ '<' ∉ c := by
  have hm : ∀ x ∈ c, x ∉ shellMeta := fun x hx => (h x hx).1
  have hs : ∀ x ∈ c, x ∉ separators := by
    intro x hx hsep
    have hws : isWs x = true → x = ' ' := (h x hx).2
    simp only [separators, List.mem_cons, List.not_mem_nil, or_false] at hsep
    rcases hsep with rfl | rfl | rfl | rfl
    · exact hm _ hx (by simp [shellMeta])
    · exact hm _ hx (by simp [shellMeta])
    · exact hm _ hx (by simp [shellMeta])
    · exact absurd (hws (by decide)) (by decide)
  refine ⟨by simpa using segments_no_sep c [] hs, ?_, ?_, ?_, ?_⟩ <;>
    intro hx <;> exact hm _ hx (by simp [shellMeta])

/-! ## Soundness of the checker -/

theorem simple_sound (c : Cmd) (h : simple c = true) : Simple c := by
  unfold simple at h
  split at h
  · exact absurd h (by simp)
  · rename_i hbad
    have hns : NoStructure c := by
      intro ch hch
      have : badChar ch = false := by
        simp only [List.any_eq_true, not_exists, not_and, Bool.not_eq_true] at hbad
        exact hbad ch hch
      simp only [badChar, Bool.or_eq_false_iff, Bool.and_eq_false_iff, bne_eq_false_iff_eq] at this
      refine ⟨by simpa using this.1, fun hw => ?_⟩
      rcases this.2 with h1 | h1
      · simp [h1] at hw
      · simpa using h1
    refine ⟨hns, ?_⟩
    split at h
    · exact absurd h (by simp)
    · rename_i prog args hw
      refine ⟨prog, args, hw, ?_⟩
      split at h
      · exact absurd h (by simp)
      · rename_i heq
        have hne : '=' ∉ prog := by simpa using heq
        split at h
        · rename_i hr
          refine ⟨hne, fun _ => by simpa using h, ?_⟩
          intro prefixes letters hro
          -- a runner is never in the flag table
          simp only [runners, List.map, List.contains, List.elem] at hr
          simp only [runOrWrite] at hro
          split at hro <;> (try split at hro) <;> (try split at hro) <;> (try split at hro) <;>
            (try split at hro) <;> simp_all
        · rename_i hr
          have hnr : prog ∉ runners := by simpa using hr
          refine ⟨hne, fun hm => absurd hm hnr, ?_⟩
          intro prefixes letters hro
          split at h
          · rename_i hnone; rw [hnone] at hro; exact absurd hro (by simp)
          · rename_i p l hsome
            rw [hsome] at hro
            obtain ⟨rfl, rfl⟩ : p = prefixes ∧ l = letters := by simpa using hro
            split at h
            · exact absurd h (by simp)
            · rename_i hg
              refine ⟨?_, ?_⟩
              · intro ch hch hgl
                simp only [List.any_eq_true, not_exists, not_and] at hg
                exact hg ch hch (by simpa using hgl)
              · intro w hwm
                have ha : argRuns p l w = false := by
                  simp only [Bool.not_eq_true', List.any_eq_false] at h
                  simpa using h w hwm
                simp only [argRuns, Bool.or_eq_false_iff, List.any_eq_false,
                  Bool.and_eq_false_iff] at ha
                refine ⟨fun q hq hpre => ?_, fun hsc x hx hxw => ?_⟩
                · have := ha.1 q hq
                  simp [List.isPrefixOf_iff_prefix, hpre] at this
                · rcases ha.2 with h1 | h1
                  · simp [h1] at hsc
                  · have := h1 x hx
                    simp [hxw] at this

/-! ## The theorems -/

/-- The built-in `^echo( |$)` pattern, as the regex reads it. -/
def echoPat (c : Cmd) : Bool := "echo ".toList.isPrefixOf c || c == "echo".toList
/-- The built-in `^rm( |$)` pattern. -/
def rmPat (c : Cmd) : Bool := "rm ".toList.isPrefixOf c || c == "rm".toList

def builtIn : Config := { allow := [echoPat], ask := [rmPat], strict := false }

/-- Before the fix: the built-in lists allow `echo x; rm -rf ~`, and the toy
shell runs two commands from it. -/
theorem current_violates :
    classifyCurrent builtIn "echo x; rm -rf ~".toList = .allow ∧
    segments "echo x; rm -rf ~".toList [] = ["echo x".toList, " rm -rf ~".toList] ∧
    ¬ Simple "echo x; rm -rf ~".toList := by
  refine ⟨by decide, by decide, fun ⟨hns, _⟩ => ?_⟩
  exact (hns ';' (by decide)).1 (by decide)

/-- After the fix: for every config and every command, "allow" answers only a
simple command — so the toy shell runs exactly one command from it. -/
theorem fixed_invariant (cfg : Config) (c : Cmd) (h : classifyFixed cfg c = .allow) :
    Simple c ∧ segments c [] = [c] := by
  have hs : simple c = true := by
    unfold classifyFixed fallthrough at h
    split at h
    · split at h <;> cases h
    · split at h
      · cases h
      · split at h
        · rename_i hc; exact (Bool.and_eq_true _ _ ▸ hc).1
        · split at h <;> cases h
  have hS := simple_sound c hs
  exact ⟨hS, (toy_shell_one_command c hS.1).1⟩

/-- The fix only ever demotes an "allow": every other answer is unchanged. -/
theorem fixed_only_demotes (cfg : Config) (c : Cmd)
    (h : classifyFixed cfg c ≠ classifyCurrent cfg c) : classifyCurrent cfg c = .allow := by
  unfold classifyFixed classifyCurrent at *
  cases h1 : c.isEmpty <;> cases h2 : cfg.ask.any (· c) <;> cases h3 : cfg.allow.any (· c) <;>
    cases h4 : simple c <;> simp_all

/-- And a simple command gets exactly the answer it got before. -/
theorem fixed_keeps_simple (cfg : Config) (c : Cmd) (h : simple c = true) :
    classifyFixed cfg c = classifyCurrent cfg c := by
  simp [classifyFixed, classifyCurrent, h]

end Eve.Command
