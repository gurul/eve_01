/-
  Eve.AppsPolicy — what the owner's apps may do: the direct apps' read/write/ask policy (apps_mcp.py), the
  owner's yes before a write (telegram.py, consent.py), and Composio's account and toolkit limits
  (composio_tools.py). Line numbers are those of 712dd34 (2026-10-09), except in the findings below, which
  cite a35b043, the tree they were found in.

  Historical code modelled below (through fixed_invariant); the current args-aware policy is
  modelled in the "Current routine-action policy" section and code_invariant. Historical write-policy bypasses are
  not claims about the current implementation. The last section (eve lite, 2026-10-13) models
  `composio_tools.decide` whole at d419b37 with the `personal_direct` keyword eve lite passes as False, and the lite
  question before COMPOSIO_MANAGE_CONNECTIONS (`lite_invariant`).

  Code modelled:
  - apps_mcp.py:179-189 READ_VERBS, WRITE_VERBS, NOUN_TOO (copied below as lists);
    516-517 `_words`, 520-524 `name_reads`, 527-534 `tool_reads`;
    713-735 `AppsBridge.start` (the read-policy filter at 725-726; `_raw[fn["name"]] = dict(tool)` at 728, so a
    later tool of the same function name replaces an earlier one: `lookup` takes the last);
    802-804 `reads`, 806-824 `decide`, 826-836 `execute` (it runs `_raw[name]`, the same tool `decide` judged;
    830-831 refuse any name not in `_raw`).
  - telegram.py:6242-6269 `_direct_tool` and 6335-6362 `_app_tool`: refuse returns; ask builds its question with
    `_write_question` (6271-6281): an email (`gmail__send_message`, apps_mcp.py:602-603 WHOLE_ASKS) is asked
    whole, tainted or not, and `describe_whole` (624-632) returns None when the whole preview (`_email_preview`,
    610-621) is longer than `max(tool_gate.SHOW_CHARS, WHOLE_ASK_CHARS)` = 3500 characters; then `_direct_tool`
    returns `tool_gate.TOO_LONG` (6262-6264) and neither asks nor executes. Modelled as `execDirect`'s
    `askable`: whether the question fits. Otherwise ask runs `_ask_user(..., firm=True)` (6265) and executes
    only when `consent.bare_decision(answer) == "allow"` (6267); run executes (6269). `_ask_user` returns the
    typed words, "yes"/"no" for a tap (ALLOW_DENY, 1334), and "no (no answer within N seconds)" on a timeout
    (6605); a cancel raises, so nothing executes. `_app_tool` first refuses any web toolkit slug (6338-6344,
    `web_toolkit_slugs`).
  - consent.py:19-26 YES_WORDS, YES_OPENINGS, NO_WORDS, HOLD_WORDS; 35-41 `approves`, 44-47 `denies`,
    50-52 `decision`, 58-61 BARE_*, 64-72 `bare_decision`.
  - composio_tools.py:75 DIRECT_TOOLKITS, 81 OTHER_ACCOUNT_TOOLKITS, 57 PRIMARY_ALIAS; 179-184 `is_read_only`
    (a slug's `reads` here); 242-287 `workbench_slugs` (`some` slugs when it can judge the script, `none` when it
    must ask); 290-307 `consequential_slugs`; 316-330 `_personal_account_slugs`; 333-357 `decide`;
    520-556 `_allowed_toolkits`; 558-562 `multi_account`; 485-518 `start` (the session is made with
    `toolkits={"enable": allowed}`, or `{"disable": self._disabled}` when `allowed` is None, 497).

  Properties (over arbitrary tools, policies, calls, answers, configurations and account listings):
  - (P1) under a read policy no tool that writes is executed: a direct tool executes as a read only if its
    readOnlyHint is true AND its name has no writing verb (a noun-too word writes only as the first word);
  - (P2) a write under an ask policy executes only after a bare allow answer to a question that fit: no answer,
    "ok but ...", a no, a timeout execute nothing, and an email whose whole preview does not fit the limit is
    refused unasked, whatever the owner would have answered;
  - (P3) Composio never executes a Gmail/Google Calendar item naming no account or the personal one, nor a
    workbench script that calls them; an account aliased personal (in any case, with spaces) or unaliased never
    makes Gmail/Calendar join the session; Drive and GitHub never join; and when Gmail or Calendar does join,
    the session is multi-account (every call must name its account);
  - (P4) Composio's verdict for a mixed call is the strictest of its parts' verdicts (refuse > ask > run).

  Findings (`current_violates`; replays in bridge/tests/test_lean_appspolicy_replay.py, each failing on a35b043):
  1. P1. `tool_reads` lets a tool with NO readOnlyHint read by its name alone (503-513, the `name_reads`
     fallback). The words are a closed list, so a tool named `find_and_replace` (no hint) is offered and run under
     Gmail's read policy: "replace" is no writing verb. The module docstring (26-27) states the rule P1 states.
     Fix: no hint is not a read (`fixReads`).
  2. P3. `_allowed_toolkits` compares the raw alias with "" and "personal" (496), while `decide` lowercases and
     strips the account (305). A personal Gmail aliased "Personal" joins the session.
  3. P3. A Gmail or Calendar named in EVE_COMPOSIO_TOOLKITS joins (485) even when only the personal account
     is connected, and then `multi_account` is False (no account recorded), so the session is not made with
     explicit selection.
  4. P3. EVE_COMPOSIO_TOOLKITS=all (`toolkits` None) makes a session with no toolkit limit (470-471,
     506-507): Drive and GitHub join (and their accounts are recorded, 494), and so does a personal-only Gmail.
  5. P2, P3. `workbench_slugs` (232-268) is a denylist that does not block `__builtins__` (nor `open` with
     `pd.read_pickle`): `b = __builtins__; (b['exec'] if isinstance(b, dict) else b.exec)("run_composio_tool(
     'GMAIL_SEND_EMAIL', ...)")` is judged `[]`, so `decide` says run and the hidden call runs with no question,
     whatever Gmail's policy (`current_violates_workbench`). Fix (`fixJudge`): a name or import starting with "_",
     any name, attribute or import holding "pickle", the keyword `allow_pickle`, pandas' `read_hdf`/`HDFStore`,
     `eval`/`exec`/`query` as attributes, and `operator.attrgetter`/`methodcaller` make `workbench_slugs` return
     None, so the script asks. `open` stays allowed: scripts read Composio's saved results with it; what makes a
     file run code is the loader, and those are blocked.
  Fix (`fixSession`): the alias is normalised as `decide` normalises an account; Gmail/Calendar stay in the list
  only with an active non-personal account; DIRECT_TOOLKITS' accounts are never recorded; with "all" the session
  is made with `toolkits={"disable": [googledrive, github, and Gmail/Calendar without a non-personal account]}`
  (the SDK's ToolRouterToolkitsDisableConfig, composio 0.22.0 tool_router.py:94-97).
  P4 holds on the code as it is (`p4_*`); P2 and the call half of P3 hold on it for every call with no hidden
  workbench call (`p2_composio`, `p3_calls`), and on the fixed judge for every call (`fixed_hidden_asks`,
  `p3_calls_fixed`).
  Applied on 2026-10-05 (feat/apps-mcp): `tool_reads` returns `hint is True and not writes` (`fixReads`);
  `_allowed_toolkits` / `start` build `fixSession` (alias `strip().lower()`, `unheld` Gmail/Calendar dropped from
  the enable list, `{"disable": DIRECT_TOOLKITS | unheld}` for "all"); `workbench_slugs` blocks what `fixJudge`'s
  contract names. All five replays pass on the fixed code.

  Where the Python goes beyond the model:
  - Words. A tool name is modelled as its word list; `_words` (split on "_" and "-", lowercased) is not. A
    camelCase name is one word, so a mislabelled `sendEmail` with readOnlyHint true would read (P1 trusts the hint
    for words it cannot see). A slug's read/write is the bool `reads` (`is_read_only`, the same word rule upper-cased,
    plus its shape regex), its toolkit the abstract `Kit` (`slug_words(...)[0]`, lower-cased).
  - Answers. An answer is its word list; consent's `_WORD` regex (letters, one inner apostrophe, lowercased,
    curly apostrophe folded) is not modelled. YES_OPENINGS is matched as a word-list prefix; on a bare answer
    (every word in the bare set) this equals the Python's joined-string `startswith`, since the only bare words
    that begin with "it", "ahead", "for", "good", "course" are those words.
  - Accounts. Python's `strip()` and `lower()` are Unicode; the model's `norm` strips space, tab, CR, LF and
    lowercases ASCII. An account named by the personal connection's id (not its alias) is not "the personal one"
    here: whether Composio accepts a connected-account id in "account" is outside the model.
  - The workbench judge's soundness. `fixJudge` assumes the fixed `workbench_slugs` returns None for every script
    that calls a tool it does not list. It is a denylist over Python's AST, so that is assumed, not proved: the
    model proves what follows from it.
  - The workbench. A script `workbench_slugs` cannot judge (`none`) asks, and a bare allow runs it; such a script
    could call Gmail. P3 for it rests on Composio's explicit account selection (the fixed session is multi-account
    whenever Gmail or Calendar joins, `fixed_invariant`), which a script cannot satisfy. COMPOSIO_REMOTE_BASH_TOOL
    and a multi-execute item whose slug is a code tool (`Item.code`, modelled) likewise only ask.
  - `_multi_execute_tools` (202-204) reads no item when `tools` is not a list, so `decide` says run; the SDK
    (composio 0.22.0 tool_router_session.py `_route_multi_execute`) forwards such a payload to the backend as it is.
    The model's `multi items` assumes the items `decide` reads are the items Composio runs.
  - Taint. Once a turn has read outside text (`self._taint`), a write the policy runs is asked anyway
    (`tainted_write`, telegram.py:6257 and 6350-6351), through the same `_write_question` gate: whole up to
    SHOW_CHARS, or refused unasked. The model's `.write` runs with no question, so under taint the Python
    executes in a subset of the model's cases; every theorem here takes "executes" as its hypothesis, so each
    still holds of the tainted code. The question's text is not modelled: that an email's question holds every
    recipient, the subject and the whole body is `_email_preview`'s, outside this model.
  - The toolkit of a slug is its first word. A bundle toolkit holding Gmail or Calendar actions under another
    prefix (Composio's `googlesuper`) is an `app` here: it is not in OTHER_ACCOUNT_TOOLKITS, so neither `decide`'s
    account refusal nor the session's account rule applies to it. P3 holds only while no such toolkit is connected.
  - Every name that is not a multi-execute, workbench or bash call is a `metaTool` with no consequential part
    (`decide` runs it). That the session's tools are only Composio's meta tools, and that
    COMPOSIO_MANAGE_CONNECTIONS changes nothing the owner must approve, is the SDK's behaviour, not modelled.
  - An empty `tool_slug` in a multi-execute item is skipped by `decide` (301-303) and left to Composio to refuse;
    a non-mapping item likewise (204). The direct apps' function names are clipped to 64 characters (566); the
    model's `name` is the clipped name, and `decide` and `execute` both look up the same `_raw` entry.
  - Two callers reach `AppsBridge.execute` with no `decide` and no question: meet.py:967 (`calendar__list_events`,
    for the notetaker) and `_load_repos` (apps_mcp.py:737-747, from `start` at 734-735; github_repos.py:76-80
    calls `github__get_me` and `github__search_repositories`). `execute` refuses any name not in `_raw`
    (830-831), and under a read policy `_raw` holds only offered tools, so what can run this way is a hinted,
    verb-free tool (`exec_only_offered`). Under a write or ask policy the same path could run a write; those two
    callers name reads, which is checked by their tests, not here.
  - A failed or partial account listing (548-549) is modelled as the prefix read before the failure.
-/
namespace Eve.AppsPolicy

/-! ## Words (apps_mcp.py:179-189, 516-534) -/

def READ_VERBS : List String :=
  ["get", "list", "fetch", "search", "find", "read", "retrieve", "lookup", "check", "count", "describe", "query",
   "view", "download", "show", "preview", "suggest"]

def WRITE_VERBS : List String :=
  ["send", "create", "delete", "remove", "update", "edit", "modify", "post", "publish", "merge", "transfer",
   "submit", "cancel", "archive", "trash", "untrash", "reply", "forward", "invite", "share", "add", "set", "move",
   "upload", "insert", "write", "patch", "put", "execute", "run", "trigger", "mark", "unmark", "label", "unlabel",
   "assign", "close", "revoke", "grant", "rename", "copy", "import", "sync", "reset", "clear", "purge", "draft",
   "compose", "schedule", "accept", "decline", "approve", "reject", "respond", "react", "apply", "fork", "push",
   "dismiss", "lock", "unlock", "star", "unstar", "pin", "unpin",
   "pay", "buy", "purchase", "charge", "withdraw", "book", "order", "deploy"]

def NOUN_TOO : List String := ["label", "draft", "run", "schedule", "star", "pin", "lock", "order"]

/-- `writes` in `name_reads` / `tool_reads`: a writing verb, where a noun-too word counts only first. -/
def writesFrom (first : Bool) : List String → Bool
  | [] => false
  | w :: ws => (WRITE_VERBS.contains w && (!NOUN_TOO.contains w || first)) || writesFrom false ws

def hasWrite (ws : List String) : Bool := writesFrom true ws

/-- `name_reads` (520-524). -/
def nameReads (ws : List String) : Bool := ws.any READ_VERBS.contains && !hasWrite ws

/-- The server's readOnlyHint: true, false, or anything else (absent, not a bool, annotations not a dict). -/
inductive Hint where
  | yes
  | no
  | absent
  deriving DecidableEq, Repr

structure Tool where
  name : String                -- the function name, `<app>__<tool>`, clipped as function_tool clips it
  hint : Hint
  words : List String          -- `_words` of the server's tool name
  deriving DecidableEq, Repr

/-- `tool_reads` at a35b043 (503-513 there): with no hint, the name decides. -/
def curReads (t : Tool) : Bool :=
  match t.hint with
  | .yes => !hasWrite t.words
  | .no => false
  | .absent => nameReads t.words

/-- `tool_reads`, fixed (527-534): only a true hint reads, and only when the name has no writing verb. -/
def fixReads (t : Tool) : Bool :=
  match t.hint with
  | .yes => !hasWrite t.words
  | _ => false

/-! ## Policy and verdicts -/

inductive Pol where
  | read
  | write
  | ask
  deriving DecidableEq, Repr

inductive Verdict where
  | run
  | ask
  | refuse
  deriving DecidableEq, Repr

/-! ## The owner's answer (consent.py) -/

def YES_WORDS : List String :=
  ["yes", "y", "yeah", "yep", "yup", "ya", "ok", "okay", "k", "sure", "go", "allow", "allowed", "approve",
   "approved", "confirm", "confirmed", "affirmative", "proceed", "absolutely", "definitely"]
def YES_OPENINGS : List (List String) :=
  [["do", "it"], ["go", "ahead"], ["go", "for", "it"], ["sounds", "good"], ["of", "course"]]
def NO_WORDS : List String :=
  ["no", "n", "nope", "nah", "deny", "denied", "stop", "don't", "dont", "block", "cancel", "decline", "reject",
   "never", "negative"]
def HOLD_WORDS : List String :=
  ["no", "not", "nope", "nah", "don't", "dont", "never", "stop", "wait", "cancel", "hold", "deny", "block", "but",
   "unless", "later"]
def BARE_EXTRA : List String := ["please", "thanks", "thank", "you", "leave", "it", "that", "this", "one", "now"]
def BARE_WORDS : List String := YES_WORDS ++ NO_WORDS ++ BARE_EXTRA ++ YES_OPENINGS.flatten

def opens (ws : List String) : Bool := YES_OPENINGS.any (fun o => o.isPrefixOf ws)

/-- `approves` (35-41). -/
def approves : List String → Bool
  | [] => false
  | w :: ws => (YES_WORDS.contains w || opens (w :: ws)) && !(w :: ws).any HOLD_WORDS.contains

/-- `denies` (44-47). -/
def denies : List String → Bool
  | [] => false
  | w :: _ => NO_WORDS.contains w

inductive Ans where
  | allow
  | deny
  | none
  deriving DecidableEq, Repr

/-- `bare_decision` (64-72). -/
def bareDecision (ws : List String) : Ans :=
  if ws = [] ∨ ws.length > 4 ∨ ws.any (fun w => !BARE_WORDS.contains w) then .none
  else if approves ws then .allow else if denies ws then .deny else .none

/-! ## The direct apps (apps_mcp.py `start`, `decide`, `execute`; telegram.py `_direct_tool`) -/

/-- `start` (725-726): under a read policy only the tools that read are offered. -/
def offered (reads : Tool → Bool) (p : Pol) (listed : List Tool) : List Tool :=
  listed.filter (fun t => match p with | .read => reads t | _ => true)

/-- `_raw[name]` (728): the last offered tool with that function name. -/
def lookup (name : String) (l : List Tool) : Option Tool := l.reverse.find? (fun t => t.name == name)

/-- `AppsBridge.decide` (806-824). -/
def decideDirect (reads : Tool → Bool) (p : Pol) (listed : List Tool) (name : String) : Verdict :=
  match lookup name (offered reads p listed) with
  | none => .refuse
  | some t => if reads t then .run else match p with
      | .read => .refuse
      | .write => .run
      | .ask => .ask

/-- `_direct_tool` (6242-6269) then `execute` (826-836): the tool that runs, if any. `askable` is whether
`_write_question` (6271-6281) returns a question: false when the whole email preview is longer than the limit
(`describe_whole` None, apps_mcp.py:624-632), on which `_direct_tool` returns TOO_LONG (6262-6264) and asks
nothing; the answer is then whatever the owner would have said, and nothing executes. -/
def execDirect (reads : Tool → Bool) (p : Pol) (listed : List Tool) (name : String) (ans : List String)
    (askable : Bool) : Option Tool :=
  match decideDirect reads p listed name with
  | .refuse => none
  | .ask => if askable = true ∧ bareDecision ans = .allow then lookup name (offered reads p listed) else none
  | .run => lookup name (offered reads p listed)

/-! ## Composio (composio_tools.py) -/

/-- A toolkit. `web n` are WEB_TOOLKITS, `app n` every other toolkit (slack, notion, linear, ...). -/
inductive Kit where
  | gmail
  | googlecalendar
  | googledrive
  | github
  | web (n : Nat)
  | app (n : Nat)
  deriving DecidableEq, Repr

/-- OTHER_ACCOUNT_TOOLKITS (81). -/
def other : Kit → Bool
  | .gmail | .googlecalendar => true
  | _ => false

/-- DIRECT_TOOLKITS (75). -/
def direct : Kit → Bool
  | .googledrive | .github => true
  | _ => false

def isWeb : Kit → Bool
  | .web _ => true
  | _ => false

def isWs (c : Char) : Bool := c = ' ' || c = '\t' || c = '\n' || c = '\r'
def lowerA (c : Char) : Char := if 'A' ≤ c ∧ c ≤ 'Z' then Char.ofNat (c.toNat + 32) else c
/-- `str(x).strip().lower()` (ASCII). -/
def norm (s : List Char) : List Char := (((s.dropWhile isWs).reverse.dropWhile isWs).reverse).map lowerA

/-- `account in ("", PRIMARY_ALIAS)` after `strip().lower()` (composio_tools.py:324-325). -/
def personalName (a : List Char) : Bool := norm a == [] || norm a == "personal".toList

/-- One COMPOSIO_MULTI_EXECUTE_TOOL item: its slug's toolkit, whether the slug only reads, whether the slug is a
code tool (RUNS_CODE: it always asks, 348), its "account". -/
structure Item where
  kit : Kit
  reads : Bool
  code : Bool
  account : List Char
  deriving DecidableEq, Repr

/-- A slug a workbench script calls through run_composio_tool. -/
structure Slug where
  kit : Kit
  reads : Bool
  deriving DecidableEq, Repr

/-- A Composio call: a multi-execute, a workbench script (`workbench_slugs`: `some` slugs, or `none` when it
cannot judge the script; `hidden` are the slugs the script really calls that `workbench_slugs` does not see, e.g.
through `__builtins__`'s exec), the remote bash tool, or another meta tool (search, schemas, connections), which
`consequential_slugs` gives no slug. -/
inductive Call where
  | multi (items : List Item)
  | workbench (script : Option (List Slug)) (hidden : List Slug)
  | bash
  | metaTool
  deriving Repr

/-- `_personal_account_slugs` (316-330): the toolkits of the items/slugs that reach the owner's own account. -/
def personalParts : Call → List Kit
  | .multi items => (items.filter (fun i => other i.kit && personalName i.account)).map (·.kit)
  | .workbench s _ => ((s.getD []).filter (fun x => other x.kit)).map (·.kit)
  | _ => []

/-- A part of `consequential_slugs`: a code tool (asks) or a writing slug of a toolkit. -/
inductive Part where
  | code
  | slug (k : Kit)
  deriving DecidableEq, Repr

/-- `consequential_slugs` (290-307). -/
def consequential : Call → List Part
  | .workbench none _ => [.code]
  | .workbench (some l) _ => (l.filter (fun x => !x.reads)).map (fun x => .slug x.kit)
  | .bash => [.code]
  | .multi items => (items.filter (fun i => i.code || !i.reads)).map (fun i => if i.code then .code else .slug i.kit)
  | .metaTool => []

/-- One part's rule (348-349): a code tool asks; a slug follows its toolkit's policy. -/
def ruleOf (pol : Kit → Pol) : Part → Verdict
  | .code => .ask
  | .slug k => match pol k with
      | .read => .refuse
      | .write => .run
      | .ask => .ask

/-- `decide` (333-357), as it is. -/
def decideC (pol : Kit → Pol) (c : Call) : Verdict :=
  if personalParts c ≠ [] then .refuse else
  let vs := (consequential c).map (ruleOf pol)
  if vs.contains .refuse then .refuse else if vs.contains .ask then .ask else .run

/-- `web_toolkit_slugs` nonempty (84-93): `_app_tool` refuses before `decide`. -/
def webRefused : Call → Bool
  | .multi items => items.any (fun i => isWeb i.kit)
  | _ => false

/-- `_app_tool` (6335-6362): whether Composio's `execute` runs the call. Its ask goes through the same
`_write_question` gate as `_direct_tool`'s (6352-6357); no Composio name is in WHOLE_ASKS, so untainted the
question is the clipped line and always fits: `askable` is not threaded here. -/
def execC (pol : Kit → Pol) (c : Call) (ans : List String) : Bool :=
  if webRefused c then false else
  match decideC pol c with
  | .refuse => false
  | .ask => bareDecision ans == .allow
  | .run => true

/-! ### The session (`_allowed_toolkits`, `multi_account`, `start`) -/

/-- One connected account as `connected_accounts.list` returns it. -/
structure Conn where
  kit : Kit
  active : Bool
  alias : List Char            -- `str(data.get("alias") or "")`
  id : List Char
  deriving DecidableEq, Repr

/-- What the session is made with: no toolkit limit, `{"enable": l}`, or `{"disable": l}`. -/
inductive Allow where
  | all
  | enable (l : List Kit)
  | disable (l : List Kit)
  deriving Repr

def joined : Allow → Kit → Bool
  | .all, _ => true
  | .enable l, k => l.contains k
  | .disable l, k => !l.contains k

structure Session where
  allow : Allow
  multi : Bool
  deriving Repr

/-- `multi_account` (558-562) over the recorded (toolkit, name) pairs. -/
def multiOf (accts : List (Kit × List Char)) : Bool :=
  accts.any (fun a => other a.1 || decide ((accts.filter (fun b => b.1 = a.1)).length > 1))

/-- The recorded name (545-547): the alias, or the id when there is none; nothing when both are empty. -/
def nameOf (c : Conn) : Option (Kit × List Char) :=
  let n := if c.alias = [] then c.id else c.alias
  if n = [] then none else some (c.kit, n)

/-- `_allowed_toolkits`' test for one account at a35b043 (493-497 there): the raw alias. -/
def curAdmit (cfg : Option (List Kit)) (c : Conn) : Bool :=
  c.active && (cfg.isNone || !direct c.kit) && !(other c.kit && (c.alias == [] || c.alias == "personal".toList))

/-- The session `start` made at a35b043 (456-473, 481-515 there). -/
def curSession (cfg : Option (List Kit)) (conns : List Conn) : Session :=
  let adm := conns.filter (curAdmit cfg)
  { allow := match cfg with
      | none => .all
      | some c => .enable ((c ++ adm.map (·.kit)).filter (fun k => !isWeb k && !direct k))
    multi := multiOf (adm.filterMap nameOf) }

/-- The fixed test for one account (537-542): active, not a direct app's, and for Gmail/Calendar not the
personal one (the alias normalised exactly as `decide` normalises an account). -/
def fixAdmit (c : Conn) : Bool :=
  c.active && !direct c.kit && !(other c.kit && personalName c.alias)

/-- The fixed session (497, 551-556): Gmail/Calendar stay only with an admitted account; "all" disables the direct
apps and any Gmail/Calendar without one. -/
def fixSession (cfg : Option (List Kit)) (conns : List Conn) : Session :=
  let adm := conns.filter fixAdmit
  let has (k : Kit) : Bool := (adm.map (·.kit)).contains k
  { allow := match cfg with
      | none => .disable ([Kit.googledrive, .github] ++ ([Kit.gmail, .googlecalendar].filter (fun k => !has k)))
      | some c => .enable ((c ++ adm.map (·.kit)).filter (fun k => !isWeb k && !direct k && (!other k || has k)))
    multi := multiOf (adm.filterMap nameOf) }

/-- P3 for a session: Drive and GitHub never join; Gmail/Calendar join only with an active account that is not the
personal one, and then the session is multi-account. -/
def SessionOk (conns : List Conn) (s : Session) : Prop :=
  joined s.allow .googledrive = false ∧ joined s.allow .github = false ∧
  ∀ k, other k = true → joined s.allow k = true →
    (∃ c ∈ conns, c.kit = k ∧ c.active = true ∧ personalName c.alias = false) ∧ s.multi = true

/-- P3 for a call: no Gmail/Calendar item naming no account or the personal one, and no visible workbench slug of
Gmail/Calendar. -/
def CallOk : Call → Prop
  | .multi items => ∀ i ∈ items, other i.kit = true → personalName i.account = false
  | .workbench (some l) h => ∀ x ∈ l ++ h, other x.kit = false
  | _ => True

/-- The slugs a workbench script calls unseen. -/
def hiddenCalls : Call → List Slug
  | .workbench _ h => h
  | _ => []

/-- The fixed `workbench_slugs`: a script that hides a call is not judged (`none`), so it asks. This is the fix's
contract (a name starting with "_" or a pickle/HDF loader makes it return None); that a denylist over Python catches
every way to hide a call is assumed here, not proved. -/
def fixJudge : Call → Call
  | .workbench (some l) (x :: xs) => .workbench none (x :: xs)
  | c => c

def execF (pol : Kit → Pol) (c : Call) (ans : List String) : Bool := execC pol (fixJudge c) ans

/-! ## The counterexamples (the code as it is) -/

def replaceTool : Tool := ⟨"gmail__find_and_replace", .absent, ["find", "and", "replace"]⟩

/-- Finding 1 (P1): Gmail is read only, the server lists `find_and_replace` with no readOnlyHint; it is offered,
`decide` says run, and it executes with no answer at all. -/
theorem current_violates_p1 :
    execDirect curReads .read [replaceTool] "gmail__find_and_replace" [] true = some replaceTool ∧
      replaceTool.hint ≠ .yes := by
  decide

/-- Finding 2 (P3): the personal Gmail aliased "Personal" joins the session. -/
theorem current_violates_alias :
    let conns := [Conn.mk .gmail true "Personal".toList "ca_1".toList]
    joined (curSession (some []) conns).allow .gmail = true ∧
      ¬ ∃ c ∈ conns, c.kit = .gmail ∧ c.active = true ∧ personalName c.alias = false := by
  refine ⟨by decide, ?_⟩
  intro ⟨c, hc, _, _, hp⟩
  simp at hc; subst hc; revert hp; decide

/-- Finding 3 (P3): Gmail named in EVE_COMPOSIO_TOOLKITS joins with only the personal account connected,
and the session is not multi-account. -/
theorem current_violates_config :
    let s := curSession (some [.gmail]) [Conn.mk .gmail true "personal".toList "ca_1".toList]
    joined s.allow .gmail = true ∧ s.multi = false := by
  decide

/-- Finding 4 (P3): with "all", GitHub (and Drive) join. -/
theorem current_violates_all :
    joined (curSession none [Conn.mk .github true [] "ca_1".toList]).allow .github = true ∧
      joined (curSession none []).allow .googledrive = true := by
  decide

/-- Finding 5 (P2, P3): a workbench script that calls GMAIL_SEND_EMAIL through `__builtins__`' exec is judged
`some []` ("only computes"); under Gmail's read policy, with no answer at all, it runs. -/
theorem current_violates_workbench :
    let c := Call.workbench (some []) [⟨.gmail, false⟩]
    execC (fun k => if k = .gmail then .read else .ask) c [] = true ∧ ¬ CallOk c := by
  refine ⟨by decide, ?_⟩
  intro h
  have := h ⟨.gmail, false⟩ (by simp)
  revert this; decide

theorem current_violates :
    (execDirect curReads .read [replaceTool] "gmail__find_and_replace" [] true = some replaceTool ∧
      replaceTool.hint ≠ .yes) ∧
    ¬ SessionOk [Conn.mk .gmail true "Personal".toList "ca_1".toList]
        (curSession (some []) [Conn.mk .gmail true "Personal".toList "ca_1".toList]) ∧
    ¬ SessionOk [Conn.mk .gmail true "personal".toList "ca_1".toList]
        (curSession (some [.gmail]) [Conn.mk .gmail true "personal".toList "ca_1".toList]) ∧
    ¬ SessionOk [Conn.mk .github true [] "ca_1".toList] (curSession none [Conn.mk .github true [] "ca_1".toList]) ∧
    (execC (fun k => if k = .gmail then .read else .ask) (.workbench (some []) [⟨.gmail, false⟩]) [] = true ∧
      ¬ CallOk (.workbench (some []) [⟨.gmail, false⟩])) := by
  refine ⟨current_violates_p1, ?_, ?_, ?_, current_violates_workbench⟩
  · intro ⟨_, _, h⟩
    have := (h .gmail rfl (by decide)).1
    obtain ⟨c, hc, _, _, hp⟩ := this
    simp at hc; subst hc; revert hp; decide
  · intro ⟨_, _, h⟩
    have := (h .gmail rfl (by decide)).2
    revert this; decide
  · intro ⟨_, h, _⟩
    revert h; decide

/-! ## Proofs: the answer -/

theorem bare_allow_shape (ws : List String) (h : bareDecision ws = .allow) :
    ws ≠ [] ∧ ws.length ≤ 4 ∧ (∀ w ∈ ws, BARE_WORDS.contains w = true ∧ HOLD_WORDS.contains w = false) ∧
      ∃ w rest, ws = w :: rest ∧ (YES_WORDS.contains w = true ∨ opens ws = true) := by
  unfold bareDecision at h
  split at h
  · cases h
  · rename_i hb
    simp only [not_or] at hb
    obtain ⟨hne, hlen, hany⟩ := hb
    cases hw : ws with
    | nil => exact absurd hw hne
    | cons w rest =>
        rw [hw] at h hany hlen
        by_cases ha : approves (w :: rest) = true
        · simp only [approves, Bool.and_eq_true, Bool.or_eq_true, Bool.not_eq_true'] at ha
          obtain ⟨hy, hh⟩ := ha
          refine ⟨by simp, by omega, ?_, w, rest, rfl, hy⟩
          intro x hx
          refine ⟨?_, ?_⟩
          · have := hany; simp only [List.any_eq_true, Bool.not_eq_true', not_exists, not_and] at this
            have h2 := this x hx; simpa using h2
          · rw [List.any_eq_false] at hh
            simpa using hh x hx
        · simp [ha] at h; split at h <;> simp at h

/-- No answer, a timeout, "ok but", "ok, and what's the weather", "yes wait", a no: none is a bare allow. -/
theorem p2_not_allow :
    bareDecision [] ≠ .allow ∧
    bareDecision ["no", "no", "answer", "within", "seconds"] ≠ .allow ∧
    bareDecision ["ok", "but"] ≠ .allow ∧
    bareDecision ["ok", "and", "what's", "the", "weather", "tomorrow"] ≠ .allow ∧
    bareDecision ["yes", "wait"] ≠ .allow ∧
    bareDecision ["no"] ≠ .allow ∧ bareDecision ["deny"] ≠ .allow ∧
    bareDecision ["yes"] = .allow ∧ bareDecision ["go", "ahead", "please"] = .allow := by
  decide

/-! ## Proofs: the direct apps -/

theorem lookup_mem (name : String) (l : List Tool) (t : Tool) (h : lookup name l = some t) : t ∈ l := by
  unfold lookup at h
  exact List.mem_reverse.mp (List.mem_of_find?_eq_some h)

theorem mem_offered_read (reads : Tool → Bool) (l : List Tool) (t : Tool) (h : t ∈ offered reads .read l) :
    reads t = true := by
  unfold offered at h
  exact (List.mem_filter.mp h).2

/-- Under any policy, a tool that executes was offered, and it either reads, or the policy writes, or the question
fit and the owner's answer was a bare allow under an ask policy. This is the gate of `_direct_tool`, for every
`reads`. -/
theorem exec_direct_gate (reads : Tool → Bool) (p : Pol) (listed : List Tool) (name : String) (ans : List String)
    (askable : Bool) (t : Tool) (h : execDirect reads p listed name ans askable = some t) :
    t ∈ offered reads p listed ∧
      (reads t = true ∨ p = .write ∨ (p = .ask ∧ askable = true ∧ bareDecision ans = .allow)) := by
  unfold execDirect decideDirect at h
  cases hl : lookup name (offered reads p listed) with
  | none => simp [hl] at h
  | some u =>
      have hu := lookup_mem _ _ _ hl
      simp only [hl] at h
      by_cases hr : reads u = true
      · simp only [hr, ↓reduceIte, Option.some.injEq] at h; subst h; exact ⟨hu, Or.inl hr⟩
      · simp only [hr, Bool.false_eq_true, ↓reduceIte] at h
        cases p with
        | read => simp at h
        | write => simp at h; subst h; exact ⟨hu, Or.inr (Or.inl rfl)⟩
        | ask =>
            simp only at h
            by_cases ha : askable = true ∧ bareDecision ans = .allow
            · simp only [ha, and_self, ↓reduceIte, Option.some.injEq] at h; subst h
              exact ⟨hu, Or.inr (Or.inr ⟨rfl, ha⟩)⟩
            · simp [ha] at h

/-- P2 on the code as it is: under an ask policy a tool that does not read executes only when its question fit
and after a bare allow. An email too long to show whole executes under no answer. -/
theorem p2_direct (reads : Tool → Bool) (listed : List Tool) (name : String) (ans : List String) (askable : Bool)
    (t : Tool) (h : execDirect reads .ask listed name ans askable = some t) (hw : reads t = false) :
    askable = true ∧ bareDecision ans = .allow := by
  obtain ⟨_, h | h | ⟨_, h⟩⟩ := exec_direct_gate reads .ask listed name ans askable t h
  · simp [hw] at h
  · cases h
  · exact h

/-- The whole-ask refusal (2026-10-09): an email whose preview does not fit executes under no answer at all, even
"yes"; the positive control is the same call with a question that fits. -/
theorem too_long_never_executes (reads : Tool → Bool) (listed : List Tool) (name : String) (ans : List String)
    (hw : ∀ t, lookup name (offered reads .ask listed) = some t → reads t = false) :
    execDirect reads .ask listed name ans false = none := by
  unfold execDirect decideDirect
  cases hl : lookup name (offered reads .ask listed) with
  | none => rfl
  | some u => simp [hw u hl]

theorem fixReads_spec (t : Tool) (h : fixReads t = true) : t.hint = .yes ∧ hasWrite t.words = false := by
  unfold fixReads at h
  cases hh : t.hint <;> simp [hh] at h
  exact ⟨rfl, h⟩

/-- Under a read policy, anything `execute` can run (it runs only names in `_raw`, i.e. offered tools) is a hinted,
verb-free tool, with or without `decide` and a question: the callers that skip `decide` (meet.py:967,
`_load_repos`) can run only reads there. -/
theorem exec_only_offered (name : String) (listed : List Tool) (t : Tool)
    (h : lookup name (offered fixReads .read listed) = some t) : t.hint = .yes ∧ hasWrite t.words = false :=
  fixReads_spec t (mem_offered_read _ _ _ (lookup_mem _ _ _ h))

/-- P1, fixed: under a read policy, whatever the server lists and whatever the owner answers, a tool that executes
has readOnlyHint true and no writing verb in its name. -/
theorem p1_fixed (listed : List Tool) (name : String) (ans : List String) (askable : Bool) (t : Tool)
    (h : execDirect fixReads .read listed name ans askable = some t) : t.hint = .yes ∧ hasWrite t.words = false := by
  obtain ⟨hm, _⟩ := exec_direct_gate fixReads .read listed name ans askable t h
  exact fixReads_spec t (mem_offered_read _ _ _ hm)

/-- P1, fixed, for every policy: a tool that runs with no allow and no write policy is a hinted read. -/
theorem p1_fixed_any (p : Pol) (listed : List Tool) (name : String) (ans : List String) (askable : Bool) (t : Tool)
    (h : execDirect fixReads p listed name ans askable = some t) (hp : p ≠ .write)
    (ha : bareDecision ans ≠ .allow) : t.hint = .yes ∧ hasWrite t.words = false := by
  obtain ⟨_, h | h | ⟨_, _, h⟩⟩ := exec_direct_gate fixReads p listed name ans askable t h
  · exact fixReads_spec t h
  · exact absurd h hp
  · exact absurd h ha

/-! ## Proofs: Composio's verdict -/

def rank : Verdict → Nat
  | .run => 0
  | .ask => 1
  | .refuse => 2

def vmax (a b : Verdict) : Verdict := if rank a ≥ rank b then a else b

/-- The strictest verdict of a list (run for none). -/
def strictest (vs : List Verdict) : Verdict := vs.foldr vmax .run

theorem strictest_mem_eq (vs : List Verdict) :
    strictest vs = if Verdict.refuse ∈ vs then .refuse else if Verdict.ask ∈ vs then .ask else .run := by
  induction vs with
  | nil => rfl
  | cons v rest ih =>
      have e : strictest (v :: rest) = vmax v (strictest rest) := rfl
      rw [e, ih]
      cases v <;> by_cases hr : Verdict.refuse ∈ rest <;> by_cases ha : Verdict.ask ∈ rest <;>
        simp [vmax, rank, hr, ha]

theorem strictest_eq (vs : List Verdict) :
    strictest vs = if vs.contains .refuse then .refuse else if vs.contains .ask then .ask else .run := by
  rw [strictest_mem_eq]; simp

theorem strictest_ge (vs : List Verdict) (v : Verdict) (h : v ∈ vs) : rank v ≤ rank (strictest vs) := by
  rw [strictest_eq]
  by_cases hr : vs.contains .refuse = true
  · simp only [hr, ↓reduceIte]; cases v <;> simp [rank]
  · by_cases ha : vs.contains .ask = true
    · simp only [hr, ha, ↓reduceIte, Bool.false_eq_true]
      cases v with
      | refuse => exact absurd (List.elem_eq_true_of_mem h) hr
      | _ => simp [rank]
    · simp only [hr, ha, ↓reduceIte, Bool.false_eq_true]
      cases v with
      | refuse => exact absurd (List.elem_eq_true_of_mem h) hr
      | ask => exact absurd (List.elem_eq_true_of_mem h) ha
      | run => simp [rank]

theorem strictest_mem (vs : List Verdict) (h : vs ≠ []) : strictest vs ∈ vs := by
  rw [strictest_eq]
  by_cases hr : vs.contains .refuse = true
  · simp only [hr, ↓reduceIte]; exact List.mem_of_elem_eq_true hr
  · by_cases ha : vs.contains .ask = true
    · simp only [hr, ha, ↓reduceIte, Bool.false_eq_true]; exact List.mem_of_elem_eq_true ha
    · simp only [hr, ha, ↓reduceIte, Bool.false_eq_true]
      obtain ⟨v, hv⟩ := List.exists_mem_of_ne_nil vs h
      cases v with
      | refuse => exact absurd (List.elem_eq_true_of_mem hv) hr
      | ask => exact absurd (List.elem_eq_true_of_mem hv) ha
      | run => exact hv

/-- Each part's own verdict: an item or slug reaching the personal account refuses; the writing parts follow
`ruleOf`. -/
def partVerdicts (pol : Kit → Pol) (c : Call) : List Verdict :=
  (personalParts c).map (fun _ => .refuse) ++ (consequential c).map (ruleOf pol)

/-- P4: `decide` is the strictest of its parts' verdicts (run when no part changes anything). -/
theorem p4_strictest (pol : Kit → Pol) (c : Call) : decideC pol c = strictest (partVerdicts pol c) := by
  rw [strictest_eq]
  unfold decideC partVerdicts
  by_cases hp : personalParts c = []
  · simp only [hp, ne_eq, not_true_eq_false, ↓reduceIte, List.map_nil, List.nil_append]
  · have this : ((personalParts c).map (fun _ => Verdict.refuse) ++ (consequential c).map (ruleOf pol)).contains
        .refuse = true := by
      obtain ⟨k, hk⟩ := List.exists_mem_of_ne_nil _ hp
      apply List.elem_eq_true_of_mem
      exact List.mem_append_left _ (List.mem_map.mpr ⟨k, hk, rfl⟩)
    simp only [ne_eq, hp, not_false_eq_true, ↓reduceIte, this]

theorem p4_ge (pol : Kit → Pol) (c : Call) (v : Verdict) (h : v ∈ partVerdicts pol c) :
    rank v ≤ rank (decideC pol c) := by
  rw [p4_strictest]; exact strictest_ge _ _ h

theorem p4_mem (pol : Kit → Pol) (c : Call) :
    decideC pol c ∈ partVerdicts pol c ∨ (partVerdicts pol c = [] ∧ decideC pol c = .run) := by
  rw [p4_strictest]
  by_cases h : partVerdicts pol c = []
  · right; rw [h]; exact ⟨rfl, rfl⟩
  · left; exact strictest_mem _ h

/-- P2 for Composio, on the code as it is: a call with a writing part that does not run by policy executes only
after a bare allow. -/
theorem p2_composio (pol : Kit → Pol) (c : Call) (ans : List String) (h : execC pol c ans = true)
    (hask : decideC pol c = .ask) : bareDecision ans = .allow := by
  unfold execC at h
  by_cases hw : webRefused c = true
  · simp [hw] at h
  · simp only [hw, Bool.false_eq_true, ↓reduceIte, hask, beq_iff_eq] at h
    exact h

/-- P3 for calls, on the code as it is: whatever the policy and the answer, a call Composio executes names no
personal account for Gmail/Calendar and its visible workbench slugs call neither. -/
theorem p3_calls (pol : Kit → Pol) (c : Call) (ans : List String) (h : execC pol c ans = true)
    (hh : hiddenCalls c = []) : CallOk c := by
  have hp : personalParts c = [] := by
    by_cases hne : personalParts c = []
    · exact hne
    · exfalso
      have hr : decideC pol c = .refuse := by
        unfold decideC; simp only [ne_eq, hne, not_false_eq_true, ↓reduceIte]
      unfold execC at h
      simp [hr] at h
  cases c with
  | multi items =>
      intro i hi ho
      simp only [personalParts, List.map_eq_nil_iff, List.filter_eq_nil_iff, Bool.and_eq_true, not_and] at hp
      have := hp i hi ho
      simpa using this
  | workbench s hid =>
      cases s with
      | none => trivial
      | some l =>
          simp only [hiddenCalls] at hh; subst hh
          intro x hx
          rw [List.append_nil] at hx
          simp only [personalParts, Option.getD_some, List.map_eq_nil_iff, List.filter_eq_nil_iff] at hp
          simpa using hp x hx
  | bash => trivial
  | metaTool => trivial

/-- Fixed: a script that hides a call runs only after a bare allow, whatever the policy. -/
theorem fixed_hidden_asks (pol : Kit → Pol) (c : Call) (ans : List String) (h : execF pol c ans = true)
    (hh : hiddenCalls c ≠ []) : bareDecision ans = .allow := by
  cases c with
  | workbench s hid =>
      simp only [hiddenCalls] at hh
      obtain ⟨x, xs, rfl⟩ := List.exists_cons_of_ne_nil hh
      have hj : fixJudge (.workbench s (x :: xs)) = .workbench none (x :: xs) := by
        cases s <;> rfl
      unfold execF at h; rw [hj] at h
      unfold execC at h
      simp [webRefused, decideC, personalParts, consequential, ruleOf] at h
      exact h
  | multi _ => simp [hiddenCalls] at hh
  | bash => simp [hiddenCalls] at hh
  | metaTool => simp [hiddenCalls] at hh

theorem fixJudge_hidden (c : Call) (hh : hiddenCalls c = []) : fixJudge c = c := by
  cases c with
  | workbench s hid =>
      simp only [hiddenCalls] at hh; subst hh; cases s <;> rfl
  | _ => rfl

/-- Fixed P3 for calls: a call the fixed code runs with no hidden call names no personal account and calls no
Gmail/Calendar from a judged script; one with a hidden call asked first (`fixed_hidden_asks`). -/
theorem p3_calls_fixed (pol : Kit → Pol) (c : Call) (ans : List String) (h : execF pol c ans = true)
    (hh : hiddenCalls c = []) : CallOk c := by
  unfold execF at h; rw [fixJudge_hidden c hh] at h
  exact p3_calls pol c ans h hh

/-- A multi-execute item whose slug is a code tool asks whatever its toolkit's ("composio")
policy and whatever the slug's words say (329: `"ask" if slug in RUNS_CODE`). -/
theorem code_item_asks (pol : Kit → Pol) (n : Nat) (r : Bool) (acct : List Char) :
    decideC pol (.multi [⟨.app n, r, true, acct⟩]) = .ask := by
  simp [decideC, personalParts, consequential, ruleOf, other]

/-! ## Proofs: the fixed session -/

theorem nameOf_some (c : Conn) (h : c.alias ≠ []) : nameOf c = some (c.kit, c.alias) := by
  simp [nameOf, h]

theorem personalName_nil : personalName [] = true := by decide

theorem multiOf_of_other (accts : List (Kit × List Char)) (a : Kit × List Char) (ha : a ∈ accts)
    (ho : other a.1 = true) : multiOf accts = true := by
  unfold multiOf
  exact List.any_eq_true.mpr ⟨a, ha, by simp [ho]⟩

theorem mem_admitted (conns : List Conn) (k : Kit)
    (h : ((conns.filter fixAdmit).map (·.kit)).contains k = true) : ∃ c ∈ conns, fixAdmit c = true ∧ c.kit = k := by
  obtain ⟨c, hc, hck⟩ := List.mem_map.mp (List.mem_of_elem_eq_true h)
  obtain ⟨hc, hadm⟩ := List.mem_filter.mp hc
  exact ⟨c, hc, hadm, hck⟩

/-- Every Gmail/Calendar that joins the fixed session has an admitted account. -/
theorem fixed_join_other (cfg : Option (List Kit)) (conns : List Conn) (k : Kit) (ho : other k = true)
    (hj : joined (fixSession cfg conns).allow k = true) : ∃ c ∈ conns, fixAdmit c = true ∧ c.kit = k := by
  cases cfg with
  | none =>
      cases k <;> simp [other] at ho <;> simp [fixSession, joined] at hj <;>
        obtain ⟨x, h1, h2, h3⟩ := hj <;> exact ⟨x, h1, h2, h3⟩
  | some c =>
      simp only [fixSession, joined] at hj
      obtain ⟨_, hok⟩ := List.mem_filter.mp (List.mem_of_elem_eq_true hj)
      simp only [ho, Bool.not_true, Bool.false_or, Bool.and_eq_true] at hok
      exact mem_admitted conns k hok.2

theorem fixed_session_ok (cfg : Option (List Kit)) (conns : List Conn) : SessionOk conns (fixSession cfg conns) := by
  refine ⟨?_, ?_, ?_⟩
  · cases cfg <;> simp [fixSession, joined, direct]
  · cases cfg <;> simp [fixSession, joined, direct]
  · intro k ho hj
    obtain ⟨c, hc, hadm, hck⟩ := fixed_join_other cfg conns k ho hj
    have hoc : other c.kit = true := hck ▸ ho
    have h2 := hadm
    simp only [fixAdmit, Bool.and_eq_true, hoc, Bool.true_and, Bool.not_eq_true'] at h2
    obtain ⟨⟨hact, _⟩, hpn⟩ := h2
    refine ⟨⟨c, hc, hck, hact, hpn⟩, ?_⟩
    have hal : c.alias ≠ [] := by intro h; rw [h, personalName_nil] at hpn; cases hpn
    apply multiOf_of_other _ (c.kit, c.alias) _ hoc
    exact List.mem_filterMap.mpr ⟨c, List.mem_filter.mpr ⟨hc, hadm⟩, nameOf_some c hal⟩

/-- The same counterexamples, fixed: `find_and_replace` with no hint does not run under a read policy; "Personal",
a personal-only Gmail in the configured list, and "all" keep Gmail/GitHub/Drive out. -/
theorem fixed_replays_counterexamples :
    execDirect fixReads .read [replaceTool] "gmail__find_and_replace" [] true = none ∧
    joined (fixSession (some []) [Conn.mk .gmail true "Personal".toList "ca_1".toList]).allow .gmail = false ∧
    joined (fixSession (some [.gmail]) [Conn.mk .gmail true "personal".toList "ca_1".toList]).allow .gmail = false ∧
    joined (fixSession none [Conn.mk .github true [] "ca_1".toList]).allow .github = false ∧
    -- the work account still joins, multi-account
    (let s := fixSession (some []) [Conn.mk .gmail true "work".toList "ca_2".toList]
     joined s.allow .gmail = true ∧ s.multi = true) ∧
    -- a hinted read still runs (get_draft: a noun-too word not first), a hinted trash and label_thread do not
    (let t := Tool.mk "gmail__get_draft" .yes ["get", "draft"]
     execDirect fixReads .read [t] "gmail__get_draft" [] true = some t) ∧
    execDirect fixReads .read [⟨"gmail__trash_thread", .yes, ["trash", "thread"]⟩] "gmail__trash_thread" [] true
      = none ∧
    execDirect fixReads .read [⟨"gmail__label_thread", .yes, ["label", "thread"]⟩] "gmail__label_thread" [] true
      = none ∧
    -- the whole ask (2026-10-09): send_message under Gmail's ask policy runs on a bare yes when the question fits,
    -- and not when the whole email is too long to show (TOO_LONG, no question), even on "yes"
    (let t := Tool.mk "gmail__send_message" .no ["send", "message"]
     execDirect fixReads .ask [t] "gmail__send_message" ["yes"] true = some t ∧
     execDirect fixReads .ask [t] "gmail__send_message" ["yes"] false = none ∧
     execDirect fixReads .ask [t] "gmail__send_message" ["ok", "but"] true = none) := by
  decide

/-- The fixed code, for every input: P1 (every policy, every listing, every answer, and the `decide`-less path
through `execute`), P2 (direct, with the question that must fit, and Composio), P3 (every call Composio executes;
every session for every configuration and listing), P4 (every call). -/
theorem fixed_invariant :
    (∀ listed name ans askable t, execDirect fixReads .read listed name ans askable = some t →
        t.hint = .yes ∧ hasWrite t.words = false) ∧
    (∀ name listed t, lookup name (offered fixReads .read listed) = some t →
        t.hint = .yes ∧ hasWrite t.words = false) ∧
    (∀ listed name ans askable t, execDirect fixReads .ask listed name ans askable = some t → fixReads t = false →
        askable = true ∧ bareDecision ans = .allow) ∧
    (∀ pol c ans, execF pol c ans = true → decideC pol (fixJudge c) = .ask → bareDecision ans = .allow) ∧
    (∀ pol c ans, execF pol c ans = true → hiddenCalls c ≠ [] → bareDecision ans = .allow) ∧
    (∀ pol c ans, execF pol c ans = true → hiddenCalls c = [] → CallOk c) ∧
    (∀ cfg conns, SessionOk conns (fixSession cfg conns)) ∧
    (∀ pol c, decideC pol c = strictest (partVerdicts pol c)) :=
  ⟨p1_fixed, exec_only_offered, fun l n a k t h hw => p2_direct fixReads l n a k t h hw,
   fun pol c ans h hask => p2_composio pol (fixJudge c) ans h hask, fixed_hidden_asks, p3_calls_fixed,
   fixed_session_ok, p4_strictest⟩

/-! ## Current routine-action policy (2026-10-09)

Models apps_mcp.routine_write / AppsBridge.prepare / AppsBridge.decide(name, args) and composio_tools.decide.
The name/JSON adapters supply the classification below: only enumerated operations,
allowed keys, a private destination, and understood delivery controls produce routine=true.
Unknown args (including workbench writes), unknown operations and code are never routine.
Exact provider JSON schemas and the adapter's normalized-key extraction are covered by
Python request/policy tests, not assumed to be a theorem about a remote service.

The earlier Pol/decideDirect/ruleOf model above is retained as historical proof evidence.
The current policy has auto, and write is no longer a bypass for consequential actions.
The caller runs an asked operation only after a whole question fits and a bare allow.
-/

inductive CurrentPol where
  | read | auto | write | ask
  deriving DecidableEq, Repr

inductive RoutineKind where
  | draft | newPrivateFile | privateRepo | calendarCreate | directCalendarEdit | composioCalendarEdit | unknown
  deriving DecidableEq, Repr

structure RoutineArgs where
  known : Bool
  fieldsAllowed : Bool
  privateTarget : Bool
  noParticipants : Bool
  noDeliveryRequested : Bool
  notifyGuestsFalse : Bool
  sendUpdatesNone : Bool
deriving DecidableEq, Repr

/-- AppsBridge.prepare pins false only for an eligible edit with an unset flag and a verified
complete guest-free raw event. Eligibility includes exact REST adapter, offered tool, auto/write,
matching event ID and routine arguments. Python tests cover that extraction from the raw resource. -/
def prepareNotification (original : Option Bool) (eligible verifiedPrivate : Bool) : Option Bool :=
  if original.isNone && eligible && verifiedPrivate then some false else original

/-- google_rest._send_updates: an explicit flag wins over the event's possibly changed guests. -/
def calendarDelivery (notification : Option Bool) (guestsNow : Bool) : Bool :=
  notification.getD guestsNow

theorem prepare_explicit_notification_preserved (flag eligible verifiedPrivate : Bool) :
    prepareNotification (some flag) eligible verifiedPrivate = some flag := by
  simp [prepareNotification]

theorem prepare_ineligible_unchanged (original : Option Bool) (verifiedPrivate : Bool) :
    prepareNotification original false verifiedPrivate = original := by
  simp [prepareNotification]

theorem prepare_unverified_unchanged (original : Option Bool) (eligible : Bool) :
    prepareNotification original eligible false = original := by
  simp [prepareNotification]

theorem prepare_private_edit_no_delivery_after_guest_change (guestsNow : Bool) :
    calendarDelivery (prepareNotification none true true) guestsNow = false := by
  simp [prepareNotification, calendarDelivery]

/-- The proof boundary of routine_write: the closed operation list and validated argument facts.
Privacy and the two edit paths use their actual adapter key; ignored aliases do not qualify. -/
def routineAllowed (kind : RoutineKind) (a : RoutineArgs) : Bool :=
  a.known && a.fieldsAllowed && a.privateTarget &&
    match kind with
    | .draft | .newPrivateFile | .privateRepo => true
    | .calendarCreate => a.noParticipants && a.noDeliveryRequested
    | .directCalendarEdit => a.noParticipants && a.noDeliveryRequested && a.notifyGuestsFalse
    | .composioCalendarEdit => a.noParticipants && a.noDeliveryRequested && a.sendUpdatesNone
    | .unknown => false

/-- A single offered direct app call, or a single Composio batch part, after account restrictions.
For Composio: missing account/personal-account, read policy and malformed inputs contribute
refuse/ask verdicts before strictest; code and uninspectable write args supply routine=false. -/
def decideCurrent (present reads : Bool) (p : CurrentPol) (routine : Bool) : Verdict :=
  if !present then .refuse else if reads then .run else
    match p with
    | .read => .refuse
    | .ask => .ask
    | .auto | .write => if routine then .run else .ask

/-- Every asked act is shown whole. Oversize or negative/qualified answers execute nothing. -/
def executeCurrent (v : Verdict) (askable : Bool) (answer : List String) : Bool :=
  match v with
  | .refuse => false
  | .run => true
  | .ask => askable && (bareDecision answer == .allow)

theorem current_read_only (routine : Bool) : decideCurrent true false .read routine = .refuse := by
  simp [decideCurrent]

theorem current_explicit_ask (routine : Bool) : decideCurrent true false .ask routine = .ask := by
  simp [decideCurrent]

theorem current_unknown_args (kind : RoutineKind) (a : RoutineArgs) (h : a.known = false) :
    routineAllowed kind a = false := by
  simp [routineAllowed, h]

theorem current_unknown_operation (a : RoutineArgs) : routineAllowed .unknown a = false := by
  simp [routineAllowed]

theorem current_private_target_required (kind : RoutineKind) (a : RoutineArgs) (h : a.privateTarget = false) :
    routineAllowed kind a = false := by
  simp [routineAllowed, h]

theorem current_calendar_send_stays_guarded (kind : RoutineKind) (a : RoutineArgs)
    (hkind : kind = .calendarCreate ∨ kind = .directCalendarEdit ∨ kind = .composioCalendarEdit)
    (h : a.noParticipants = false ∨ a.noDeliveryRequested = false) : routineAllowed kind a = false := by
  rcases hkind with rfl | rfl | rfl <;> rcases h with h | h <;> simp [routineAllowed, h]

theorem current_danger_asks (p : CurrentPol) (h : p = .auto ∨ p = .write) :
    decideCurrent true false p false = .ask := by
  rcases h with rfl | rfl <;> simp [decideCurrent]

theorem current_routine_runs (p : CurrentPol) (h : p = .auto ∨ p = .write) :
    decideCurrent true false p true = .run := by
  rcases h with rfl | rfl <;> simp [decideCurrent]

theorem current_ask_requires_whole_allow (askable : Bool) (answer : List String)
    (h : executeCurrent .ask askable answer = true) : askable = true ∧ bareDecision answer = .allow := by
  simpa [executeCurrent] using h

theorem current_batch_retains_strictest (vs : List Verdict) (v : Verdict) (h : v ∈ vs) :
    rank v ≤ rank (strictest vs) := strictest_ge vs v h

/-- Current policy invariants: read-only and explicit ask remain strict, routine auto/write runs,
consequential auto/write asks, and an asked act executes only after full review and bare assent. -/
theorem code_invariant :
    (∀ routine, decideCurrent true false .read routine = .refuse) ∧
    (∀ routine, decideCurrent true false .ask routine = .ask) ∧
    (∀ p, (p = .auto ∨ p = .write) → decideCurrent true false p true = .run) ∧
    (∀ p, (p = .auto ∨ p = .write) → decideCurrent true false p false = .ask) ∧
    (∀ askable answer, executeCurrent .ask askable answer = true →
       askable = true ∧ bareDecision answer = .allow) ∧
    (∀ guestsNow, calendarDelivery (prepareNotification none true true) guestsNow = false) :=
  ⟨current_read_only, current_explicit_ask, current_routine_runs, current_danger_asks,
   current_ask_requires_whole_allow, prepare_private_edit_no_delivery_after_guest_change⟩

/-! ## eve lite: `personal_direct` (2026-10-13)

`composio_tools.decide(name, args, policy=None, *, personal_direct=True)` (docs/plans/eve-lite.md M2, "Composio
gate"). eve lite has no direct Gmail or Calendar (EVE_APPS=0), so the owner's own accounts are reached only through
Composio Connect, and lite does not refuse them: `_personal_account_slugs` is consulted only while `personal_direct`.
The default keeps today's `decide`.

`decide` is modelled whole, at d419b37 (composio_tools.py:333-372): a multi-execute whose items it cannot read asks
first (338-341); the personal-account refusal (342-345); a call with no consequential slug runs (346-348); each
consequential slug's rule (353-364: a code tool asks, `read` refuses, `auto` and `write` run what `routine_write`
passes and ask the rest, `ask` asks), which is `decideCurrent` above; the strictest wins (365-372). On branch
feat/one-click-variant (WP-3, not yet committed, so its code is cited by name) the keyword is the one change to it:
`personal = _personal_account_slugs(name, args) if personal_direct else []`. `decide_lite` is lite's verdict:
`decide(..., personal_direct=False)`, then a COMPOSIO_MANAGE_CONNECTIONS call it would run asks while
`manage_needs_yes(args)` is true, which is everything but a mapping of `toolkits` (a non-empty list of non-empty
strings), `reinitiate_all` absent or False and `session_id` absent or a string, the check-or-link shape of the S1
schema. Today that call runs unasked (`consequential_slugs` gives it no slug). `_app_tool` (telegram.py) refuses a web toolkit before either, takes
`decide_lite` in lite, asks a whole question that must fit (`_write_question`: SHOW_CHARS, or Gmail's whole-ask room
for a send in lite, `sends_mail`) and runs on a bare allow alone (`executeCurrent`, `askable`).

Proved: `personal_direct=True` is today's `decide` for every call (`personal_direct_default`); `False` changes the
verdict only of a call that reaches the personal account, and only ever towards run (`lite_same_off_personal`,
`lite_only_relaxes`); P4 for both values (`p4_personal_direct`); P1, a read policy refuses a write, for both values
and through `decide_lite` (`p1_personal_direct`, `p1_lite`); P2, every non-routine write, code tool, unreadable call
and unproven connection change runs in lite only after a whole question and a bare allow (`p2_lite`). P3 is given up
on purpose under `False`: there the owner's own Gmail is the only Gmail (`lite_controls`). In lite every toolkit's
policy is `auto` (`litePolicy`).

The workbench judge is the fixed one (`fixJudge`'s contract: a script that hides a call is not judged, so it asks).
Composio Connect's session is Composio's own (`ConnectBridge`, composio_connect.py): no account split, so the session
half of P3 (`fixSession`) is not lite's.
-/

/-- One multi-execute item as `decide` reads it now: its toolkit, whether its slug only reads (`is_read_only`),
whether it is a code tool (RUNS_CODE), whether `routine_write(toolkit, words, arguments)` passes (d419b37:360), and
its "account". -/
structure ItemNow where
  kit : Kit
  reads : Bool
  code : Bool
  routine : Bool
  account : List Char
  deriving DecidableEq, Repr

/-- A Composio call as `decide` sees it now. `malformed`: a multi-execute whose `tools` is not a list of mappings,
each with a non-empty string `tool_slug` (338-341). A workbench script is judged by the fixed `workbench_slugs`
(`some` slugs, or `none` when it must ask); each of its slugs is judged with no arguments (351-352), which
`routine_write` never takes for routine. `manage`: COMPOSIO_MANAGE_CONNECTIONS with `manage_needs_yes(args)`.
`metaTool`: search, schemas, wait. -/
inductive CallNow where
  | multi (items : List ItemNow)
  | malformed
  | workbench (script : Option (List Slug))
  | bash
  | manage (needsYes : Bool)
  | metaTool
  deriving Repr

/-- `_personal_account_slugs` (316-330). -/
def personalNow : CallNow → List Kit
  | .multi items => (items.filter (fun i => other i.kit && personalName i.account)).map (·.kit)
  | .workbench s => ((s.getD []).filter (fun x => other x.kit)).map (·.kit)
  | _ => []

/-- A consequential part (353-364): a code tool, or a writing slug of a toolkit with `routine_write`'s answer. -/
inductive PartNow where
  | code
  | slug (k : Kit) (routine : Bool)
  deriving DecidableEq, Repr

/-- `consequential_slugs` (290-306), each with what `decide` judges it by. -/
def consequentialNow : CallNow → List PartNow
  | .multi items => (items.filter (fun i => i.code || !i.reads)).map
      (fun i => if i.code then .code else .slug i.kit i.routine)
  | .workbench none => [.code]
  | .workbench (some l) => (l.filter (fun x => !x.reads)).map (fun x => .slug x.kit false)
  | .bash => [.code]
  | _ => []

/-- One part's verdict (357-363): a code tool's rule is "ask"; a writing slug follows its toolkit's policy and its
arguments, which is `decideCurrent` for a present tool that does not read. -/
def partNow (pol : Kit → CurrentPol) : PartNow → Verdict
  | .code => decideCurrent true false .ask false
  | .slug k r => decideCurrent true false (pol k) r

/-- The verdict of the parts' verdicts, as `decide` writes it (365-372): refuse beats ask beats run. -/
def verdictOf (vs : List Verdict) : Verdict :=
  if vs.contains .refuse then .refuse else if vs.contains .ask then .ask else .run

/-- `decide` at d419b37 (333-372). -/
def decideToday (pol : Kit → CurrentPol) : CallNow → Verdict
  | .malformed => .ask
  | c => if personalNow c ≠ [] then .refuse else verdictOf ((consequentialNow c).map (partNow pol))

/-- `decide(name, args, policy, personal_direct=pd)`: the personal refusal only while `pd`. -/
def decideP (pd : Bool) (pol : Kit → CurrentPol) : CallNow → Verdict
  | .malformed => .ask
  | c => if pd = true ∧ personalNow c ≠ [] then .refuse else verdictOf ((consequentialNow c).map (partNow pol))

/-- Lite's question before COMPOSIO_MANAGE_CONNECTIONS, alone. -/
def manageLite : CallNow → Verdict
  | .manage true => .ask
  | _ => .run

/-- `decide_lite`: `decide(..., personal_direct=False)`; a COMPOSIO_MANAGE_CONNECTIONS call it would run asks
instead while `manage_needs_yes` is true. -/
def liteVerdict (pol : Kit → CurrentPol) : CallNow → Verdict
  | .manage true => if decideP false pol (.manage true) = .run then .ask else decideP false pol (.manage true)
  | c => decideP false pol c

/-- `web_toolkit_slugs` non-empty (84-93): `_app_tool` refuses before `decide`. -/
def webNow : CallNow → Bool
  | .multi items => items.any (fun i => isWeb i.kit)
  | _ => false

/-- `_app_tool`: whether `execute` runs the call. `askable`: `_write_question` returned a question (the whole call
fits SHOW_CHARS, or Gmail's whole-ask room for a send in lite). -/
def execApp (v : Verdict) (c : CallNow) (askable : Bool) (ans : List String) : Bool :=
  !webNow c && executeCurrent v askable ans

/-- The lite policy: `toolkit_policy` with LITE_FORCED's `EVE_COMPOSIO_POLICY=gmail=auto` over
DEFAULT_TOOLKIT_POLICY (gmail, googlecalendar and googledrive `auto`), and `pol.get(toolkit, "auto")` (357) for every
other toolkit. -/
def litePolicy : Kit → CurrentPol := fun _ => .auto

/-- Each part's verdict, as P4 reads `decide`: an unreadable call is one "ask"; a personal-account item or slug is
one "refuse" while `pd`; then the consequential parts. -/
def partsNow (pd : Bool) (pol : Kit → CurrentPol) : CallNow → List Verdict
  | .malformed => [.ask]
  | c => (if pd then (personalNow c).map (fun _ => .refuse) else []) ++ (consequentialNow c).map (partNow pol)

theorem decideP_readable (pd : Bool) (pol : Kit → CurrentPol) (c : CallNow) (hc : c ≠ .malformed) :
    decideP pd pol c =
      if pd = true ∧ personalNow c ≠ [] then .refuse else verdictOf ((consequentialNow c).map (partNow pol)) := by
  cases c <;> first | exact absurd rfl hc | rfl

theorem partsNow_readable (pd : Bool) (pol : Kit → CurrentPol) (c : CallNow) (hc : c ≠ .malformed) :
    partsNow pd pol c =
      (if pd then (personalNow c).map (fun _ => .refuse) else []) ++ (consequentialNow c).map (partNow pol) := by
  cases c <;> first | exact absurd rfl hc | rfl

theorem personal_direct_default (pol : Kit → CurrentPol) (c : CallNow) : decideP true pol c = decideToday pol c := by
  cases c <;> simp [decideP, decideToday]

theorem lite_same_off_personal (pol : Kit → CurrentPol) (c : CallNow) (h : personalNow c = []) :
    decideP false pol c = decideP true pol c := by
  cases c <;> simp_all [decideP]

theorem lite_only_relaxes (pol : Kit → CurrentPol) (c : CallNow) :
    rank (decideP false pol c) ≤ rank (decideP true pol c) := by
  by_cases h : personalNow c = []
  · rw [lite_same_off_personal pol c h]; exact Nat.le_refl _
  · have ht : decideP true pol c = .refuse := by
      cases c with
      | malformed => simp [personalNow] at h
      | _ => simp [decideP, h]
    rw [ht]; cases decideP false pol c <;> simp [rank]

/-- P4, for either value of the keyword: the verdict is the strictest of the parts' verdicts. -/
theorem p4_personal_direct (pd : Bool) (pol : Kit → CurrentPol) (c : CallNow) :
    decideP pd pol c = strictest (partsNow pd pol c) := by
  rw [strictest_eq]
  by_cases hc : c = .malformed
  · subst hc; simp [decideP, partsNow]
  · rw [decideP_readable pd pol c hc, partsNow_readable pd pol c hc]
    by_cases hp : pd = true ∧ personalNow c ≠ []
    · obtain ⟨hpd, hne⟩ := hp
      have hr : ((if pd = true then (personalNow c).map (fun _ => Verdict.refuse) else []) ++
          (consequentialNow c).map (partNow pol)).contains .refuse = true := by
        obtain ⟨k, hk⟩ := List.exists_mem_of_ne_nil _ hne
        apply List.elem_eq_true_of_mem
        rw [ite_eq_left hpd]
        exact List.mem_append_left _ (List.mem_map.mpr ⟨k, hk, rfl⟩)
      rw [ite_eq_left ⟨hpd, hne⟩, ite_eq_left hr]
    · have hz : (if pd = true then (personalNow c).map (fun _ => Verdict.refuse) else []) = [] := by
        by_cases hpd : pd = true
        · have : personalNow c = [] := by
            by_cases hne : personalNow c = []
            · exact hne
            · exact absurd ⟨hpd, hne⟩ hp
          simp [hpd, this]
        · simp [hpd]
      rw [ite_eq_right hp, hz, List.nil_append]
      rfl

theorem partNow_read (pol : Kit → CurrentPol) (k : Kit) (r : Bool) (h : pol k = .read) :
    partNow pol (.slug k r) = .refuse := by
  simp [partNow, decideCurrent, h]

theorem mem_partsNow (pd : Bool) (pol : Kit → CurrentPol) (c : CallNow) (p : PartNow)
    (hp : p ∈ consequentialNow c) : partNow pol p ∈ partsNow pd pol c := by
  by_cases hc : c = .malformed
  · subst hc; simp [consequentialNow] at hp
  · rw [partsNow_readable pd pol c hc]
    exact List.mem_append_right _ (List.mem_map.mpr ⟨p, hp, rfl⟩)

/-- P1, for either value of the keyword: a call with a writing slug of a toolkit under a read policy is refused. -/
theorem p1_personal_direct (pd : Bool) (pol : Kit → CurrentPol) (c : CallNow) (k : Kit) (r : Bool)
    (hm : PartNow.slug k r ∈ consequentialNow c) (hk : pol k = .read) : decideP pd pol c = .refuse := by
  have hmem := mem_partsNow pd pol c _ hm
  rw [partNow_read pol k r hk] at hmem
  rw [p4_personal_direct, strictest_eq, ite_eq_left (List.elem_eq_true_of_mem hmem)]

theorem vmax_refuse (a b : Verdict) (h : a = .refuse ∨ b = .refuse) : vmax a b = .refuse := by
  cases a <;> cases b <;> simp_all [vmax, rank]

theorem rank_vmax (a b : Verdict) : rank a ≤ rank (vmax a b) ∧ rank b ≤ rank (vmax a b) := by
  cases a <;> cases b <;> simp [vmax, rank]

theorem rank_pos (v : Verdict) : v ≠ .run ↔ 1 ≤ rank v := by
  cases v <;> simp [rank]

theorem vmax_run (v : Verdict) : vmax v .run = v := by
  cases v <;> rfl

/-- `decide_lite` is the stricter of `decide`'s verdict and the manage question. -/
theorem liteVerdict_vmax (pol : Kit → CurrentPol) (c : CallNow) :
    liteVerdict pol c = vmax (decideP false pol c) (manageLite c) := by
  cases c with
  | manage b =>
      cases b
      · simp only [liteVerdict, manageLite]; exact (vmax_run _).symm
      · have : decideP false pol (.manage true) = .run := by
          simp [decideP, verdictOf, consequentialNow, personalNow]
        simp [liteVerdict, manageLite, this, vmax, rank]
  | _ => simp only [liteVerdict, manageLite]; exact (vmax_run _).symm

/-- Lite's P1: a read policy still refuses (whatever the manage question), so the call executes on no answer. -/
theorem p1_lite (pol : Kit → CurrentPol) (c : CallNow) (k : Kit) (r : Bool)
    (hm : PartNow.slug k r ∈ consequentialNow c) (hk : pol k = .read) (askable : Bool) (ans : List String) :
    liteVerdict pol c = .refuse ∧ execApp (liteVerdict pol c) c askable ans = false := by
  have h : liteVerdict pol c = .refuse := by
    rw [liteVerdict_vmax]; exact vmax_refuse _ _ (Or.inl (p1_personal_direct false pol c k r hm hk))
  refine ⟨h, ?_⟩
  simp [execApp, h, executeCurrent]

theorem exec_not_run (v : Verdict) (c : CallNow) (askable : Bool) (ans : List String) (hv : v ≠ .run)
    (h : execApp v c askable ans = true) : askable = true ∧ bareDecision ans = .allow := by
  unfold execApp at h
  simp only [Bool.and_eq_true] at h
  cases v with
  | run => exact absurd rfl hv
  | refuse => simp [executeCurrent] at h
  | ask => exact current_ask_requires_whole_allow askable ans h.2

/-- Lite's P2: a call that executes either runs by its verdict, or was asked as a whole question that fit and got a
bare allow; and a call with a part that does not run by itself (a write the policy does not run, a code tool), an
unreadable call, or a connection change `manage_needs_yes` does not prove, executes only after that answer. -/
theorem p2_lite (pol : Kit → CurrentPol) (c : CallNow) (askable : Bool) (ans : List String)
    (h : execApp (liteVerdict pol c) c askable ans = true) :
    (liteVerdict pol c = .run ∨ (askable = true ∧ bareDecision ans = .allow)) ∧
    ((∃ p ∈ consequentialNow c, partNow pol p ≠ .run) ∨ c = .malformed ∨ c = .manage true →
      askable = true ∧ bareDecision ans = .allow) := by
  refine ⟨?_, ?_⟩
  · by_cases hv : liteVerdict pol c = .run
    · exact Or.inl hv
    · exact Or.inr (exec_not_run _ c askable ans hv h)
  · intro hc
    apply exec_not_run _ c askable ans _ h
    rw [rank_pos, liteVerdict_vmax]
    obtain ⟨hv1, hv2⟩ := rank_vmax (decideP false pol c) (manageLite c)
    rcases hc with ⟨p, hp, hpv⟩ | rfl | rfl
    · have hge := strictest_ge _ _ (mem_partsNow false pol c p hp)
      rw [← p4_personal_direct] at hge
      have := (rank_pos _).mp hpv
      omega
    · have : rank (decideP false pol .malformed) = 1 := rfl
      omega
    · have : rank (manageLite (.manage true)) = 1 := rfl
      omega

/-- In lite every write is judged by `auto`: a routine write (a draft, a private calendar entry) runs, any other
asks, and a read runs (`litePolicy`). -/
theorem lite_policy_parts (k : Kit) (r : Bool) :
    partNow litePolicy (.slug k r) = (if r then .run else .ask) ∧ partNow litePolicy .code = .ask := by
  cases r <;> simp [partNow, litePolicy, decideCurrent]

def gmailFetch : CallNow := .multi [⟨.gmail, true, false, false, []⟩]                 -- GMAIL_FETCH_EMAILS
def gmailSend : CallNow := .multi [⟨.gmail, false, false, false, []⟩]                 -- GMAIL_SEND_EMAIL
def gmailDraft : CallNow := .multi [⟨.gmail, false, false, true, []⟩]                 -- GMAIL_CREATE_EMAIL_DRAFT
def scriptSend : CallNow := .workbench (some [⟨.gmail, false⟩])                       -- run_composio_tool(GMAIL_SEND…)
def scriptRead : CallNow := .workbench (some [⟨.gmail, true⟩])                        -- run_composio_tool(GMAIL_FETCH…)

/-- The controls. Today (`True`) the owner's own Gmail through Composio is refused, read or write; in lite (`False`)
a Gmail read naming no account runs, a draft runs (routine), a send asks and runs on a bare yes to a question that
fits, and on nothing else, and a send under a read policy is still refused. A workbench script that reads Gmail
runs, one that sends asks. COMPOSIO_MANAGE_CONNECTIONS runs today and asks in lite unless its shape is proven.
bridge/tests/test_composio_lite_decide.py (WP-3) runs the same calls through the real `decide` and `decide_lite`. -/
theorem lite_controls :
    decideP true litePolicy gmailFetch = .refuse ∧ decideP false litePolicy gmailFetch = .run ∧
    decideP true litePolicy gmailDraft = .refuse ∧ liteVerdict litePolicy gmailDraft = .run ∧
    liteVerdict litePolicy gmailSend = .ask ∧
    execApp (liteVerdict litePolicy gmailSend) gmailSend true ["yes"] = true ∧
    execApp (liteVerdict litePolicy gmailSend) gmailSend true ["ok", "but"] = false ∧
    execApp (liteVerdict litePolicy gmailSend) gmailSend false ["yes"] = false ∧
    liteVerdict (fun k => if k = .gmail then .read else .auto) gmailSend = .refuse ∧
    decideP true litePolicy scriptSend = .refuse ∧ liteVerdict litePolicy scriptSend = .ask ∧
    decideP true litePolicy scriptRead = .refuse ∧ liteVerdict litePolicy scriptRead = .run ∧
    decideToday litePolicy (.manage true) = .run ∧ liteVerdict litePolicy (.manage true) = .ask ∧
    liteVerdict litePolicy (.manage false) = .run := by
  decide

/-- eve lite's Composio gate, for every call, policy and answer: `personal_direct=True` is today's `decide`;
`False` differs only on a call that reaches the personal account, and only towards run; P4 for both; P1 (a read
policy refuses, so nothing executes); P2 (what does not run by itself executes only after a whole question that
fit and a bare allow). -/
theorem lite_invariant :
    (∀ pol c, decideP true pol c = decideToday pol c) ∧
    (∀ pol c, personalNow c = [] → decideP false pol c = decideP true pol c) ∧
    (∀ pol c, rank (decideP false pol c) ≤ rank (decideP true pol c)) ∧
    (∀ pd pol c, decideP pd pol c = strictest (partsNow pd pol c)) ∧
    (∀ pol c k r askable ans, PartNow.slug k r ∈ consequentialNow c → pol k = .read →
      execApp (liteVerdict pol c) c askable ans = false) ∧
    (∀ pol c askable ans, execApp (liteVerdict pol c) c askable ans = true →
      ((∃ p ∈ consequentialNow c, partNow pol p ≠ .run) ∨ c = .malformed ∨ c = .manage true) →
      askable = true ∧ bareDecision ans = .allow) :=
  ⟨personal_direct_default, lite_same_off_personal, lite_only_relaxes, p4_personal_direct,
   fun pol c k r askable ans hm hk => (p1_lite pol c k r hm hk askable ans).2,
   fun pol c askable ans h => (p2_lite pol c askable ans h).2⟩

end Eve.AppsPolicy
