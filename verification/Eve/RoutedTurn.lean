/-
  Eve.RoutedTurn — a Telegram turn whose toolsets the toolset router picks (2026-10-05): which tools a turn
  offers, which it runs, what its instructions name, and what it keeps from the turn before.

  The code modelled, at origin/main a35b043 (line numbers are those of that commit):
  - toolset_router.py:356-382 `route_turn`: the keyword toolsets (`keyword_toolsets`, 208), then Jev in a worker
    waited on for `timeout_s`: None, a timeout, an `Exception` (boxed as `Route(error=…)`, 371-372), a value that
    is not a `Route` (380-381), or a `Route` whose toolsets are added (382). Modelled by `Answer` and `routeTurn`.
  - telegram.py:3781-3925 `_think`: 3795 `self._lent = set()`; 3797 `self._routed = not daily and mode != "off"`;
    3798-3801 the unrouted start (a rundown's `rundown.DIRECT_APPS & up`, else `guess(words)`); 3807-3819 the routed
    start (`_direct_open = set()`, the gather of `route_turn` with `_turn_parts`, 3818 `route.toolsets & up`,
    3819 `route.toolsets & set(self._lendable())`, 3821 `_refresh_lent`); 3842 `parse_response(…,
    keep_unknown=True)`; 3858-3864 the unknown name's refusal; 3865-3866 a rundown's `rundown.allows`;
    3868 `_tool`; 3856 and 3874-3877 the refresh when `(_direct_open, _lent)` changed; 3924 the out-of-rounds
    summary's `parse_response(response, allowed)`. Modelled by `step` (`start`, `request`, `call`, `refresh`,
    `summary`).
  - telegram.py:3940-4012 `_turn_parts` (3952 `lend_blocks`, 3953 the warm-up's empty sets, 3955-3970 `allowed`
    and `AllowedNames`, 3979-4012 the blocks); 1144-1157 `AllowedNames`; 1160-1196 `parse_response`
    (1177-1179); 4015-4020 `_lendable`; 4022-4029 `_lent_block`; 4031-4039 `_refresh_lent`; 4041-4045
    `_routed_owns`; 4047-4051 `_lend`; 4053-4061 `_lent_tools`; 4063-4082 `_open_toolsets`; 4093-4109 `_warm`;
    4112-4199 `_tool` (4126-4127 open_app routed, 4128-4129 a direct app's name or one it owns, 4130-4132 a
    Composio name and `_lend(WORKSPACE)`, 4166-4168 a Share name and `_lend(SHARE)`); 4842-4874 `_app_tools`;
    4885-4904 `_direct_tool` (4894 `ensure_open`). apps_mcp.py:688 `open`, 705 `owns`, 712 `ensure_open`,
    762 `instructions`. rundown.py:126 `tools`, 133 `allows`. telegram.py:1055-1101 `request` (1072 INSTRUCTIONS,
    1081-1084 `lent_block`, 1085-1086 APPS_BLOCK), 380-432 INSTRUCTIONS (409 and 412-418: the robot body's tools),
    629-631 `own_tools`, 570 TOOL_NAMES.

  Names (`Name`): `own n` is one of eve's own tools (TOOL_NAMES) or of a service lent on every turn (memory,
  watch, lights, …); `tool k n` is tool `n` of toolset `k` (a direct app's `k__n`, Share's `share_file` /
  `find_contact`, a Composio name for the workspace); `openApp`; `other n`, a non-empty string naming nothing
  here; `blank`, a function call whose name is "" or not a string.

  The properties:
  - (P1) no tool call in a routed turn ends the turn with the failure line because of its name (`nameFail`);
  - (P2) a toolset's tool runs only if the turn's last request offered it, or its toolset is up and that call
    opened it (auto-open); a toolset that is off never runs (`ranBad`), in every mode;
  - (P3) a turn's `_direct_open` and `_lent` at its start are a function of that turn's own inputs only, never of
    the turn before (`start_forgets`); the call warm-up opens no toolset while routed (`warmBad`);
  - (P4) the router's toolsets always include the keyword toolsets, whatever Jev does (`routeFail`,
    `fixed_route_keeps_keywords`), so a routed turn opens every keyword toolset that is up (`fixed_turn_opens_keywords`);
    and a keyword-only toolset is in them exactly when the words named it (`fixed_route_keyword_only`);
  - (P5) every tool name a routed (or unrouted, non-rundown) turn's instructions write is offered by that request
    (`fixed_prompt_offers`). Instructions are modelled as the set of names they write (`named`).

  Findings at a35b043 (`current_violates`), each replayed against the real Python in
  bridge/tests/test_lean_routedturn_replay.py (each replay failed at a35b043):
  1. (P4) `route_turn` read `box["route"]` after the worker; a router raising a `BaseException` (the worker
     catches only `Exception`) left the box empty and `route_turn` raised KeyError. `frozenset(r.toolsets)` was
     unguarded, so a `Route` whose toolsets are not a set of names raised TypeError. The gather in `_think`
     re-raised: FAILED_LINE, and the keyword toolsets were lost. (`routeTrace`, `malformedTrace`)
  2. (P1) `parse_response(…, keep_unknown=True)` still raised on a call whose name was "" or not a string
     (1178): FAILED_LINE. (`blankTrace`)
  3. (P1) the out-of-rounds summary (3924) parsed without `keep_unknown`, so a function call the model made
     despite tool_choice "none", to a name not offered, raised: FAILED_LINE. (`summaryTrace`)
  4. (P5) INSTRUCTIONS (412-418, and 409's take_photo) name the robot body's tools on every turn, but
     `own_tools` offers them only with EVE_ROBOT=1. (`current_violates`, own 7)
  Found by the adversarial check of this model (same day), each replayed in the same test file:
  5. (P4) `route_turn` passes Jev's `r.ms` through unchecked (382) and `_think` formats it in `route.log_line()`
     (3820, `{self.ms:.0f}`) after the gather: a `Route` whose ms is None raised TypeError, FAILED_LINE, keywords
     lost. A guard around `frozenset(r.toolsets)` alone does not reach it. (`msTrace`, `Answer.unloggable`)
  6. (P5) APPS_BLOCK_REST (585-590) names web_search whenever Composio's tools ride, but websearch.tools_for
     offers nothing with EVE_WEB_SEARCH=off. (`current_violates`, own 2 with `wsOpen`)
  7. (P5, router off) `_turn_parts` (4001) writes `accounts_note` whenever app_tools is non-empty, not only while
     Composio's session is up; ComposioBridge.start fills `_accounts` before `sessions.create`, so a failed (or
     still running) start leaves the note naming COMPOSIO_MULTI_EXECUTE_TOOL with no Composio tool offered.
     (`current_violates`, `cfgDown`)
  The fix (`Fix`): `route_turn` treats a missing box entry and an unreadable `Route` (its toolsets, or an ms that
  is not a number) as an answer that adds nothing; `parse_response` keeps any name as unknown (refused as its
  result); the summary parses with `keep_unknown=True`; the robot body's paragraph is written only when
  `robot_enabled()`; web_search is named only when a search is offered; unrouted, `accounts_note` rides only while
  `_composio_up()`.
  Applied on 2026-10-05 (feat/apps-mcp): `route_turn` reads `box.get("route")` and the Route's toolsets, `float(ms)`
  and error inside one `try` (anything unreadable is "not a route", keywords kept); `parse_response` keeps a blank
  or non-string name as unknown ""; the summary parses with `keep_unknown=True`; INSTRUCTIONS is the text without
  the robot body's paragraph and camera sentence (INSTRUCTIONS_ROBOT with EVE_ROBOT=1); APPS_BLOCK_REST_NO_WEB
  names no web_search when `websearch.tools_for` is empty; unrouted, `accounts_note` rides only while
  `_composio_up()` and a COMPOSIO_ tool is in app_tools (a narrower condition than the model's, so P5 still holds).
  All nine replays pass.
  P2 and P3 held at a35b043 for every trace (`current_p2_p3_invariant`, `start_forgets`).
  2026-10-09 (712dd34): an eighth toolset, Google Contacts (`Kit.contacts`), a direct app (apps_mcp.py:100, one
  read tool) that is keyword-only (toolset_router.py:69-70 KEYWORD_ONLY): `_words` (222-229) returns names in
  `TOOLSETS | KEYWORD_ONLY`, so the keywords can carry it, and `route_turn` masks Jev's toolsets to `TOOLSETS`
  (410), so Jev never adds it. `Fix.routeTurn` carries the mask (`!k.keywordOnly`); `fixed_route_keyword_only`
  proves a keyword-only toolset is open exactly when the words named it, for every answer, and
  `jev_adds_routed_toolsets` is the control (Jev still adds a routed one). `fixed_turn_opens_keywords` covers it
  as any keyword toolset. `Cur.routeTurn` models a35b043, which had no mask and no Contacts; that it would let Jev
  add `.contacts` is the model's domain being today's, not a finding at a35b043. rundown.DIRECT_APPS (rundown.py:18)
  is still (gmail, calendar): `rundownApp .contacts = false`.

  Where the Python goes beyond the model (stated):
  - P5 is not proved for a rundown, and does not hold there in either version (`rundown_prompt_gap`): a rundown's
    payload tools are `rundown.tools(app_tools)` alone, yet INSTRUCTIONS names eve's own tools and APPS_BLOCK
    names COMPOSIO_MANAGE_CONNECTIONS and web_search. A call to one is refused by `rundown.allows`, never failed.
  - Eve's own tools are outside P2: TOOL_NAMES accepts the robot body's tools even when they are not offered
    (EVE_ROBOT off), and `_tool_take_photo` / `_robot_tool` then run when `_on_photo`, `_head` or `_scene`
    is wired (daemon.py:1381-1383 wires them). The handlers refuse when the hardware is absent.
  - P5 assumes Composio's tool list, while its session is up, holds every name APPS_BLOCK and `accounts_note`
    write (`metas ⊆ has workspace`). `_lent_tools` returns [] when `self._apps.tools()` raises, and then the
    lent block still names COMPOSIO_MULTI_EXECUTE_TOOL.
  - `config.about` (self_context.py) and the other services' blocks (watch, lights, …) are taken to name only
    their own tools, offered with them (`inBase`); web_search is not in INSTRUCTIONS nor a robot tool (`hweb`,
    checked by a regex over INSTRUCTIONS against TOOLS).
  - PROFILE_HEADER / MEMORY_HINT / TODAY_HEADER name memory_search and memory_read; they are offered whenever a
    Memory is lent, except when `Memory.tools()` raises (`_memory_tools` returns []). Not modelled.
  - parse_response still fails a turn on a call whose arguments are not a JSON object, or that has no call_id:
    not because of its name, so outside P1. A tool result that is not JSON-serialisable fails it at json.dumps.
  - The `/opentable` command calls `_tool` outside a turn (4267); opentable_search reads no per-turn state.
  - The direct apps' policy (`decide`: refuse / ask / run) is abstracted: a direct tool "runs" when the app has
    it; the policy only narrows that. `accounts_note` is taken to name its tool whenever the workspace is open
    (in the code only while some app has two accounts): a stronger P5.
  - A response is parsed whole before any call runs; the model parses and runs call by call. P1 and P2 do not
    depend on that order.
  - `known` is exact for routed turns (the `allowed` built from the empty-start `_turn_parts`, grown by
    `_refresh_lent`, plus `_routed_owns`); other services' names are `own` and always known.
  - The timeout, the Jev error string and the log line are not modelled: a timeout is an `Answer` that adds
    nothing. Rounds are not counted: `summary` is the out-of-rounds call, at any point.
-/
namespace Eve.RoutedTurn

/-! ## Toolsets, names, configuration -/

inductive Kit where
  | calendar | maps | gmail | drive | github | contacts | share | workspace
  deriving DecidableEq, Repr

/-- A direct app: membership in `AppsBridge.apps` (apps_mcp.py:700-701, the connected servers of SERVERS,
95-100; telegram.py:4806 `up = set(self._direct.apps)`, 6233-6237 `_direct_up`). The other two are telegram's own
(LENT_ORDER, telegram.py:700). Google Contacts (2026-10-09) is a direct app like the five: it is opened by
`route.toolsets & up` (4830), owned and executed through AppsBridge. -/
def Kit.direct : Kit → Bool
  | .share => false
  | .workspace => false
  | _ => true

/-- toolset_router.KEYWORD_ONLY (69-70): a toolset its words alone open, never asked of Jev. `_words` (222-229)
returns names in `TOOLSETS | KEYWORD_ONLY`, so `keyword_toolsets` can carry it; `route_turn` masks Jev's set to
`TOOLSETS` (410), so Jev can never add it. -/
def Kit.keywordOnly : Kit → Bool
  | .contacts => true
  | _ => false

def kits : List Kit := [.calendar, .maps, .gmail, .drive, .github, .contacts, .share, .workspace]

/-- rundown.DIRECT_APPS. -/
def rundownApp : Kit → Bool
  | .gmail => true
  | .calendar => true
  | _ => false

abbrev KSet := Kit → Bool

def KSet.add (s : KSet) (k : Kit) : KSet := fun j => s j || decide (j = k)

/-- One turn's open toolsets: `_direct_open` (`d`) and `_lent` (`l`). -/
structure Open where
  d : KSet := fun _ => false
  l : KSet := fun _ => false

def Open.has (o : Open) (k : Kit) : Bool := if k.direct then o.d k else o.l k

inductive Name where
  | own (n : Nat)
  | tool (k : Kit) (n : Nat)
  | openApp
  | other (n : Nat)
  | blank
  deriving DecidableEq, Repr

inductive Mode where
  | routed | daily | off
  deriving DecidableEq, Repr

/-- This Mac. -/
structure Cfg where
  routerOn : Bool              -- EVE_TOOLSET_ROUTER is not off (`_toolset_mode != "off"`)
  on : Kit → Bool              -- the toolset is here: a direct app up (`_direct_up`), Share lent, Composio started
  has : Kit → Nat → Bool       -- its tool names: a direct app's offered tools (`_raw`), share.TOOL_NAMES, Composio's
  reads : Kit → Nat → Bool     -- a direct tool its server marks read only (AppsBridge.reads)
  dailyWs : Nat → Bool         -- a Composio name rundown.tools keeps (READ_META_TOOLS, MULTI_EXECUTE)
  metas : Nat → Bool           -- the Composio names APPS_BLOCK and accounts_note write
  body : Nat → Bool            -- an own tool of the robot body (ROBOT_BODY_TOOLS)
  robot : Bool                 -- EVE_ROBOT (robot_enabled)
  inBase : Nat → Bool          -- an own tool INSTRUCTIONS (or an always-on block) names
  web : Nat → Bool             -- the own name web_search (websearch.TOOL_NAME, in TOOL_NAMES)
  searchOn : Bool              -- websearch.tools_for offers a web search (EVE_WEB_SEARCH is not off)
  noted : Bool                 -- accounts_note is non-empty: ComposioBridge._accounts were listed (start, before
                               -- sessions.create) with an app on two accounts or an OTHER_ACCOUNT_TOOLKITS one,
                               -- and app_tools is non-empty; whether or not the session is up

/-! ## The router (P4) -/

/-- What the worker leaves for `route_turn` to read. -/
inductive Answer where
  | none_                      -- no router (no Jev route): `TurnToolsets(found)`
  | timeout                    -- `done.wait` timed out
  | raises                     -- an `Exception`: boxed as `Route(error=…)`, no toolsets
  | raisesBase                 -- a `BaseException` that is not an `Exception`: the box stays empty
  | notRoute                   -- a value that is not a `Route`
  | route (ts : KSet)          -- a `Route` with these toolsets (an error string or not)
  | malformed                  -- a `Route` whose toolsets `frozenset(…)` cannot read (None, unhashable items)
  | unloggable (ts : KSet)     -- a `Route` with these toolsets whose `ms` is not a number (None, a string):
                               -- passed through to `TurnToolsets.ms`, formatted by `log_line` (`{ms:.0f}`) at 3820
  deriving Inhabited

/-- `route_turn` at a35b043 and `_think`'s 3820 `route.log_line()`: `none` is a raise out of either. -/
def Cur.routeTurn (kw : KSet) : Answer → Option KSet
  | .none_ => some kw
  | .timeout => some kw
  | .raises => some kw
  | .notRoute => some kw
  | .route ts => some (fun k => kw k || ts k)
  | .raisesBase => none          -- 379 `box["route"]`: KeyError
  | .malformed => none           -- 382 `frozenset(r.toolsets)`: TypeError
  | .unloggable _ => none        -- 3820 `route.log_line()`: `f"{None:.0f}"` TypeError (a str: ValueError)

/-- The fix: a missing or unreadable answer (its toolsets or its ms) adds nothing, and Jev's toolsets are masked
to TOOLSETS (`& frozenset(TOOLSETS)`, toolset_router.py:410): a keyword-only toolset comes from the words alone. -/
def Fix.routeTurn (kw : KSet) : Answer → Option KSet
  | .route ts => some (fun k => kw k || (ts k && !k.keywordOnly))
  | _ => some kw

/-! ## The turn -/

/-- What parse_response accepts (`name in TOOL_NAMES or name in allowed`). Routed: `allowed` holds open_app
from the empty-start `_turn_parts` whenever any toolset is up, every tool offered later (`_refresh_lent`), and
`_routed_owns` (a direct app's any name, Composio's names); Share's names whenever Share is lent. Unrouted: the
app tools (open_app while a direct app is closed, Composio's), `owns`, Share's names. -/
def known (c : Cfg) (m : Mode) : Name → Bool
  | .own _ => true
  | .openApp => match m with
      | .routed => kits.any c.on
      | _ => kits.any (fun k => k.direct && c.on k)
  | .tool k n => c.on k && (k.direct || c.has k n)
  | .other _ => false
  | .blank => false

/-- rundown.allows: a direct mail / calendar read tool, or a Composio read name. -/
def allowsDaily (c : Cfg) (argsOk : Bool) : Name → Bool
  | .tool k n => if k.direct then rundownApp k && c.has k n && c.reads k n
                 else decide (k = .workspace) && c.dailyWs n && argsOk
  | _ => false

/-- `_tool` for a known name: the toolsets it opens, and whether a toolset's tool ran. -/
def dispatch (c : Cfg) (m : Mode) (o : Open) (n : Name) (asks : KSet) : Open × Bool :=
  match n with
  | .openApp =>
      match m with
      -- `_open_toolsets`: direct apps and lent toolsets alike, each only if here
      | .routed => ({ d := fun k => o.d k || (asks k && c.on k && k.direct),
                      l := fun k => o.l k || (asks k && c.on k && !k.direct) }, false)
      -- AppsBridge.open: the direct apps asked for that are up
      | _ => ({ o with d := fun k => o.d k || (asks k && c.on k && k.direct) }, false)
  | .tool k t =>
      if k.direct then
        -- `name in self._direct.names or self._direct.owns(name)`: the app is up; `ensure_open`, then `decide`
        if c.on k then ({ o with d := o.d.add k }, c.has k t) else (o, false)
      else if k = .workspace then
        -- `name in self._apps.names`: `_lend(WORKSPACE)` (routed only), then `_app_tool`
        if c.on k && c.has k t then
          ({ o with l := match m with | .routed => o.l.add k | _ => o.l }, true)
        else (o, false)
      else
        -- `name in share.TOOL_NAMES`: `_lend(SHARE)` (routed and lendable), then `_share_tool` (OFF when absent)
        if c.has k t then
          ({ o with l := match m with | .routed => (if c.on k then o.l.add k else o.l) | _ => o.l }, c.on k)
        else (o, false)
  | _ => (o, false)

/-- The tools a request offers, built from the turn's parts: `request(…, **parts)` → `tools_for`, and for a
rundown `rundown.tools(app_tools)` in place of all of them. -/
def offers (c : Cfg) (m : Mode) (o : Open) : Name → Bool
  | .own n => match m with
      | .daily => false
      | _ => (!c.body n || c.robot) && (!c.web n || c.searchOn)   -- own_tools; websearch.tools_for
  | .openApp => match m with
      | .routed => kits.any (fun k => c.on k && !o.has k)
      | .off => kits.any (fun k => k.direct && c.on k && !o.d k)
      | .daily => false
  | .tool k n => c.on k && c.has k n && (match m with
      | .routed => o.has k
      | .off => if k.direct then o.d k else true
      | .daily => if k.direct then o.d k && rundownApp k && c.reads k n
                  else decide (k = .workspace) && c.dailyWs n)
  | _ => false

/-- `_turn_parts`' `lend_blocks` for a turn (not the warm-up): Share's block rides `watch_block` only unrouted
and not a rundown. -/
def lendBlocks : Mode → Bool
  | .off => true
  | _ => false

/-- The versions. -/
structure Ver where
  route : KSet → Answer → Option KSet
  blankFails : Bool            -- parse_response raises on a blank name even with keep_unknown
  summaryStrict : Bool         -- the out-of-rounds parse has no keep_unknown
  robotGate : Bool             -- the robot paragraph is written only with EVE_ROBOT
  searchGate : Bool            -- APPS_BLOCK_REST names web_search only when a web search is offered
  notesGate : Bool             -- unrouted, accounts_note rides only while Composio's session is up

def Cur : Ver := { route := Cur.routeTurn, blankFails := true, summaryStrict := true, robotGate := false,
                   searchGate := false, notesGate := false }
def Fix : Ver := { route := Fix.routeTurn, blankFails := false, summaryStrict := false, robotGate := true,
                   searchGate := true, notesGate := true }

/-- APPS_BLOCK (+ APPS_BLOCK_REST) rides: some app tool is a COMPOSIO_ one, i.e. the workspace's tools are in
app_tools (routed: while lent). -/
def appsBlock (c : Cfg) (m : Mode) (o : Open) : Bool :=
  c.on .workspace && (match m with | .routed => o.l .workspace | _ => true)

/-- The names a request's instructions write: INSTRUCTIONS (the robot paragraph only with `robotGate`),
APPS_BLOCK_REST's web_search, Composio's names, Share's. -/
def named (v : Ver) (c : Cfg) (m : Mode) (o : Open) : Name → Bool
  | .own n => (c.inBase n && (!v.robotGate || !c.body n || c.robot)) ||
              (c.web n && appsBlock c m o && (!v.searchGate || c.searchOn))
  | .tool k n =>
      if k.direct then false     -- AppsBridge.instructions and apps_direct_line name families (`calendar__…`) only
      else if k = .workspace then
        -- routed: `_lent_block` (accounts_note) and APPS_BLOCK ride only while the workspace is lent (and
        -- `_composio_up`); unrouted: APPS_BLOCK whenever Composio's tools are in app_tools, and accounts_note
        -- (`if app_tools and not routed`, 4001) whenever it is non-empty, session up or not (`noted`)
        match m with
        | .routed => c.on k && o.l k && c.metas n
        | _ => c.metas n && (c.on k || (!v.notesGate && c.noted))
      else
        -- Share's instructions: `_lent_block` while lent (routed), or `watch_block` when `lend_blocks`
        c.on k && (o.l k || lendBlocks m) && c.has k n
  | _ => false

inductive Ev where
  /-- `_think`'s start: a rundown or not; the keyword toolsets (routed) or the words' apps (`guess`, unrouted);
  Jev's answer. -/
  | start (daily : Bool) (kw : KSet) (ans : Answer)
  | request                                      -- a round's request goes out, built from the parts
  | call (n : Name) (asks : KSet) (argsOk : Bool) -- one function call of the round's reply, parsed and run
  | refresh                                      -- the round's end: `_refresh_lent` when the sets changed
  | summary (n : Name)                           -- the out-of-rounds reply carries a function call named n
  | warm                                         -- `_warm`: a call opened
  deriving Inhabited

structure St where
  mode : Mode := .off
  cur : Open := {}             -- `_direct_open`, `_lent`
  shown : Open := {}           -- what `parts` (app_tools, lent_block) were built from
  asked : Open := {}           -- what the last request was built from
  failed : Bool := false       -- this turn ended with FAILED_LINE
  nameFail : Bool := false     -- ghost: a turn failed because of a call's name (P1 broken)
  ranBad : Bool := false       -- ghost: a toolset's tool ran neither offered nor opened by its call (P2 broken)
  routeFail : Bool := false    -- ghost: a routed turn lost its keyword toolsets (P4 broken)
  warmBad : Bool := false      -- ghost: the warm-up opened a toolset while routed (P3 broken)

/-- The open sets a turn starts with, from its own inputs only. -/
def startOpen (c : Cfg) (daily : Bool) (ts : KSet) : Open :=
  if !daily && c.routerOn then
    -- 3818-3819: `route.toolsets & up`, `route.toolsets & set(self._lendable())`
    { d := fun k => ts k && c.on k && k.direct, l := fun k => ts k && c.on k && !k.direct }
  else if daily then
    -- 3799: `set(rundown.DIRECT_APPS) & up`; 3795 `_lent = set()`
    { d := fun k => rundownApp k && c.on k, l := fun _ => false }
  else
    -- 3800: `self._direct.guess(self._turn_said) if up`
    { d := fun k => ts k && c.on k && k.direct, l := fun _ => false }

def step (v : Ver) (c : Cfg) (s : St) : Ev → St
  | .start daily kw ans =>
      let m : Mode := if daily then .daily else if c.routerOn then .routed else .off
      if !daily && c.routerOn then
        match v.route kw ans with
        | none => { s with mode := m, cur := {}, shown := {}, asked := {}, failed := true, routeFail := true }
        | some ts =>
            let o := startOpen c daily ts
            { s with mode := m, cur := o, shown := o, asked := o, failed := false }
      else
        let o := startOpen c daily kw
        { s with mode := m, cur := o, shown := o, asked := o, failed := false }
  | .request => if s.failed then s else { s with asked := s.shown }
  | .refresh => if s.failed then s else { s with shown := s.cur }
  | .call n asks argsOk =>
      if s.failed then s else
      match n with
      | .blank => if v.blankFails then { s with failed := true, nameFail := true } else s
      | _ =>
          if !known c s.mode n then s                                    -- 3858: refused as its result
          else if s.mode = .daily && !allowsDaily c argsOk n then s       -- 3865: refused as its result
          else
            let r := dispatch c s.mode s.cur n asks
            let bad := match n with
              | .tool k t => r.2 && !(c.on k && (offers c s.mode s.asked (.tool k t) || r.1.has k))
              | _ => false
            { s with cur := r.1, ranBad := s.ranBad || bad }
  | .summary n =>
      if s.failed then s else
      if v.summaryStrict && (match n with | .blank => true | _ => !known c s.mode n) then
        { s with failed := true, nameFail := true }
      else s
  | .warm =>
      -- 3953: `(set(), set()) if warm and routed else (self._direct_open, self._lent)`; nothing is written back
      let w : Open := if c.routerOn then {} else s.cur
      { s with warmBad := s.warmBad || (c.routerOn && kits.any (fun k => w.d k || w.l k)) }

def run (v : Ver) (c : Cfg) (evs : List Ev) : St := evs.foldl (step v c) {}

/-! ## The counterexamples at a35b043 -/

/-- A Mac with every toolset up, the router on and the robot off; own tool 7 is a robot body tool INSTRUCTIONS
names. -/
def cfg : Cfg :=
  { routerOn := true, on := fun _ => true, has := fun _ n => n == 0, reads := fun _ _ => true,
    dailyWs := fun n => n == 0, metas := fun n => n == 0, body := fun n => n == 7, robot := false,
    inBase := fun n => n == 7 || n == 1, web := fun n => n == 2, searchOn := false, noted := true }

/-- The same Mac with EVE_WEB_SEARCH=off (as `cfg`) and Composio's session down: it listed the owner's work
Gmail (`_accounts`), then `sessions.create` failed. -/
def cfgDown : Cfg := { cfg with on := fun k => decide (k ≠ .workspace) }

/-- The workspace lent (a routed turn whose words or Jev named Slack). -/
def wsOpen : Open := { l := fun k => decide (k = .workspace) }

def kwCal : KSet := fun k => decide (k = .calendar)
def noKits : KSet := fun _ => false

/-- "when is the dentist on my calendar" (keywords: calendar); Jev's worker raises a BaseException. -/
def routeTrace : List Ev := [.start false kwCal .raisesBase]
/-- The same; Jev answers a Route whose toolsets are None. -/
def malformedTrace : List Ev := [.start false kwCal .malformed]
/-- A routed turn; the model calls a function named "". -/
def blankTrace : List Ev := [.start false kwCal .none_, .request, .call .blank noKits true]
/-- A routed turn runs out of rounds; the summary reply calls a name the turn never offered. -/
def summaryTrace : List Ev := [.start false noKits .none_, .request, .summary (.other 3)]
/-- "when is the dentist on my calendar"; Jev answers a Route whose ms is None. -/
def msTrace : List Ev := [.start false kwCal (.unloggable noKits)]

theorem current_violates :
    (run Cur cfg routeTrace).failed = true ∧ (run Cur cfg routeTrace).routeFail = true ∧
    (run Cur cfg malformedTrace).failed = true ∧ (run Cur cfg malformedTrace).routeFail = true ∧
    (run Cur cfg blankTrace).failed = true ∧ (run Cur cfg blankTrace).nameFail = true ∧
    (run Cur cfg summaryTrace).failed = true ∧ (run Cur cfg summaryTrace).nameFail = true ∧
    (run Cur cfg msTrace).failed = true ∧ (run Cur cfg msTrace).routeFail = true ∧
    -- P5: the robot body's tool is written into a routed turn's instructions and not offered
    named Cur cfg .routed {} (.own 7) = true ∧ offers cfg .routed {} (.own 7) = false ∧
    -- P5: web_search (APPS_BLOCK_REST) is written with the workspace open and the search off
    named Cur cfg .routed wsOpen (.own 2) = true ∧ offers cfg .routed wsOpen (.own 2) = false ∧
    -- P5: unrouted, accounts_note names COMPOSIO_MULTI_EXECUTE_TOOL with Composio's session down
    named Cur cfgDown .off {} (.tool .workspace 0) = true ∧ offers cfgDown .off {} (.tool .workspace 0) = false := by
  decide

/-- The fix answers each trace: the keywords stand, the blank call and the summary's call are refused, and the
robot's tool is no longer named. -/
theorem fixed_replays :
    (run Fix cfg routeTrace).failed = false ∧ (run Fix cfg routeTrace).cur.d .calendar = true ∧
    (run Fix cfg malformedTrace).failed = false ∧ (run Fix cfg malformedTrace).cur.d .calendar = true ∧
    (run Fix cfg blankTrace).failed = false ∧ (run Fix cfg summaryTrace).failed = false ∧
    (run Fix cfg msTrace).failed = false ∧ (run Fix cfg msTrace).cur.d .calendar = true ∧
    named Fix cfg .routed {} (.own 7) = false ∧ named Fix cfg .routed wsOpen (.own 2) = false ∧
    named Fix cfgDown .off {} (.tool .workspace 0) = false := by
  decide

/-- Positive controls: the properties are not empty. A routed turn's closed Share tool is not offered, its call
runs it and opens Share (the auto-open branch of P2); Share's names ride the instructions only once it is open. -/
theorem auto_open_control :
    offers cfg .routed {} (.tool .share 0) = false ∧
    (dispatch cfg .routed {} (.tool .share 0) noKits).2 = true ∧
    (dispatch cfg .routed {} (.tool .share 0) noKits).1.has .share = true ∧
    (run Fix cfg [.start false noKits .none_, .request, .call (.tool .share 0) noKits true]).cur.has .share = true ∧
    named Fix cfg .routed {} (.tool .share 0) = false ∧
    named Fix cfg .routed { l := fun k => decide (k = .share) } (.tool .share 0) = true ∧
    -- with a search on, the fix still names web_search beside the open workspace, and offers it
    named Fix { cfg with searchOn := true } .routed wsOpen (.own 2) = true ∧
    offers { cfg with searchOn := true } .routed wsOpen (.own 2) = true ∧
    -- unrouted with Composio's session up, the fix still writes its names
    named Fix cfg .off {} (.tool .workspace 0) = true := by
  decide

/-- The rundown is outside P5, in both versions: its instructions name an own tool (and a Composio name) its
payload does not offer. -/
theorem rundown_prompt_gap :
    named Fix cfg .daily {} (.own 1) = true ∧ offers cfg .daily {} (.own 1) = false ∧
    named Fix cfg .daily {} (.tool .workspace 0) = true := by
  decide

/-! ## P4: the router keeps the keywords -/

theorem fixed_route_keeps_keywords (kw : KSet) (a : Answer) :
    ∃ ts, Fix.routeTurn kw a = some ts ∧ ∀ k, kw k = true → ts k = true := by
  cases a with
  | route ts => exact ⟨_, rfl, fun k h => by simp [h]⟩
  | none_ => exact ⟨kw, rfl, fun _ h => h⟩
  | timeout => exact ⟨kw, rfl, fun _ h => h⟩
  | raises => exact ⟨kw, rfl, fun _ h => h⟩
  | raisesBase => exact ⟨kw, rfl, fun _ h => h⟩
  | notRoute => exact ⟨kw, rfl, fun _ h => h⟩
  | malformed => exact ⟨kw, rfl, fun _ h => h⟩
  | unloggable _ => exact ⟨kw, rfl, fun _ h => h⟩

/-- A keyword-only toolset (Contacts) is exactly what the words said, whatever Jev answered: the mask at
toolset_router.py:410 keeps Jev from opening it, and the keywords keep it open (`fixed_route_keeps_keywords`). -/
theorem fixed_route_keyword_only (kw : KSet) (a : Answer) (k : Kit) (hk : k.keywordOnly = true) :
    ∃ ts, Fix.routeTurn kw a = some ts ∧ ts k = kw k := by
  cases a with
  | route ts => exact ⟨_, rfl, by simp [hk]⟩
  | none_ => exact ⟨kw, rfl, rfl⟩
  | timeout => exact ⟨kw, rfl, rfl⟩
  | raises => exact ⟨kw, rfl, rfl⟩
  | raisesBase => exact ⟨kw, rfl, rfl⟩
  | notRoute => exact ⟨kw, rfl, rfl⟩
  | malformed => exact ⟨kw, rfl, rfl⟩
  | unloggable _ => exact ⟨kw, rfl, rfl⟩

/-- The positive control: Jev's answer does add a routed toolset (the calendar) the words did not name, so the
mask is the only thing keeping Contacts out. -/
theorem jev_adds_routed_toolsets :
    ∃ ts, Fix.routeTurn noKits (.route (fun k => decide (k = .calendar) || decide (k = .contacts))) = some ts ∧
      ts .calendar = true ∧ ts .contacts = false := by
  exact ⟨_, rfl, by decide, by decide⟩

/-- A routed turn (router on, not a rundown) opens every keyword toolset that is up, whatever Jev answered. -/
theorem fixed_turn_opens_keywords (c : Cfg) (s : St) (kw : KSet) (a : Answer) (hr : c.routerOn = true) :
    (step Fix c s (.start false kw a)).failed = false ∧
    ∀ k, kw k = true → c.on k = true → (step Fix c s (.start false kw a)).cur.has k = true := by
  obtain ⟨ts, hts, hk⟩ := fixed_route_keeps_keywords kw a
  have hv : Fix.route kw a = some ts := hts
  refine ⟨?_, ?_⟩
  · simp [step, hr, hv]
  · intro k hkw hon
    have ht := hk k hkw
    simp only [step, hr, hv, Bool.not_false, Bool.true_and, ite_true]
    simp [startOpen, hr, Open.has, ht, hon]

/-! ## P3: a turn's start forgets the turn before; the warm-up opens nothing while routed -/

theorem start_forgets (v : Ver) (c : Cfg) (s s' : St) (daily : Bool) (kw : KSet) (a : Answer) :
    (step v c s (.start daily kw a)).cur = (step v c s' (.start daily kw a)).cur ∧
    (step v c s (.start daily kw a)).shown = (step v c s' (.start daily kw a)).shown ∧
    (step v c s (.start daily kw a)).mode = (step v c s' (.start daily kw a)).mode := by
  simp only [step]
  split
  · cases v.route kw a <;> simp
  · simp

theorem warm_opens_nothing (v : Ver) (c : Cfg) (s : St) (hr : c.routerOn = true) :
    (step v c s .warm).warmBad = s.warmBad ∧ (step v c s .warm).cur = s.cur := by
  simp [step, hr, kits]

/-! ## P5: what a request's instructions name, it offers (routed and unrouted turns; not a rundown) -/

theorem fixed_prompt_offers (c : Cfg) (m : Mode) (o : Open) (n : Name) (hm : m ≠ .daily)
    (hmeta : ∀ t, c.metas t = true → c.has .workspace t = true)
    (hweb : ∀ t, c.web t = true → c.inBase t = false ∧ c.body t = false) :
    named Fix c m o n = true → offers c m o n = true := by
  intro h
  cases n with
  | own t =>
      have hw := hweb t
      cases m with
      | daily => exact absurd rfl hm
      | routed =>
          cases hb : c.body t <;> cases hr : c.robot <;> cases hi : c.inBase t <;> cases hwt : c.web t <;>
            cases hs : c.searchOn <;> simp_all [named, offers, Fix, appsBlock]
      | off =>
          cases hb : c.body t <;> cases hr : c.robot <;> cases hi : c.inBase t <;> cases hwt : c.web t <;>
            cases hs : c.searchOn <;> simp_all [named, offers, Fix, appsBlock]
  | tool k t =>
      cases k <;> cases m <;> simp_all [named, offers, Kit.direct, lendBlocks, Open.has, Fix]
  | openApp => simp [named] at h
  | other t => simp [named] at h
  | blank => simp [named] at h

/-! ## P1, P2, P4 and P3 (warm) over every trace -/

def Good (s : St) : Prop :=
  s.nameFail = false ∧ s.ranBad = false ∧ s.routeFail = false ∧ s.warmBad = false

/-- A toolset's tool that a call runs is up, and offered by the last request or opened by the call itself. -/
theorem dispatch_ok (c : Cfg) (m : Mode) (o asked : Open) (k : Kit) (t : Nat) (asks : KSet) (argsOk : Bool)
    (hd : m = .daily → allowsDaily c argsOk (.tool k t) = true)
    (hran : (dispatch c m o (.tool k t) asks).2 = true) :
    c.on k = true ∧ (offers c m asked (.tool k t) = true ∨ (dispatch c m o (.tool k t) asks).1.has k = true) := by
  cases hon : c.on k <;> cases hhas : c.has k t <;> cases hrd : c.reads k t <;> cases hws : c.dailyWs t <;>
    cases k <;> cases m <;>
    simp_all [dispatch, offers, allowsDaily, Kit.direct, Open.has, KSet.add, rundownApp]

theorem call_keeps_ranBad (v : Ver) (c : Cfg) (s : St) (n : Name) (asks : KSet) (argsOk : Bool)
    (h : s.ranBad = false) : (step v c s (.call n asks argsOk)).ranBad = false := by
  simp only [step]
  split
  · exact h
  · cases n with
    | blank => cases v.blankFails <;> simp [h]
    | own t => simp only; split <;> (try split) <;> simp [h]
    | openApp => simp only; split <;> (try split) <;> simp [h]
    | other t => simp only; split <;> (try split) <;> simp [h]
    | tool k t =>
        simp only
        split
        · exact h
        · rename_i hk
          split
          · exact h
          · rename_i hdaily
            have hd : s.mode = .daily → allowsDaily c argsOk (.tool k t) = true := by
              intro hm
              cases ha : allowsDaily c argsOk (.tool k t)
              · simp [hm, ha] at hdaily
              · rfl
            simp only [h, Bool.false_or]
            cases hr : (dispatch c s.mode s.cur (.tool k t) asks).2
            · simp
            · obtain ⟨hon, hor⟩ := dispatch_ok c s.mode s.cur s.asked k t asks argsOk hd hr
              rcases hor with ho | ho <;> simp [hon, ho]

/-- P2 and the warm-up's P3 are the same in both versions. -/
theorem step_keeps_ranBad_warmBad (v : Ver) (c : Cfg) (s : St) (e : Ev)
    (h : s.ranBad = false ∧ (c.routerOn = true → s.warmBad = false)) :
    (step v c s e).ranBad = false ∧ (c.routerOn = true → (step v c s e).warmBad = false) := by
  obtain ⟨hr, hw⟩ := h
  cases e with
  | start daily kw a =>
      simp only [step]
      split
      · cases v.route kw a <;> exact ⟨hr, hw⟩
      · exact ⟨hr, hw⟩
  | request => simp only [step]; split <;> exact ⟨hr, hw⟩
  | refresh => simp only [step]; split <;> exact ⟨hr, hw⟩
  | call n asks argsOk =>
      refine ⟨call_keeps_ranBad v c s n asks argsOk hr, ?_⟩
      intro hon
      have hw' := hw hon
      simp only [step]
      split
      · exact hw'
      · cases n with
        | blank => cases v.blankFails <;> simp [hw']
        | _ => simp only; split <;> (try split) <;> simp [hw']
  | summary n =>
      simp only [step]
      split
      · exact ⟨hr, hw⟩
      · repeat' split
        all_goals exact ⟨hr, hw⟩
  | warm =>
      refine ⟨by simp [step, hr], ?_⟩
      intro hon
      simp [step, hon, hw hon, kits]

theorem fixed_step (c : Cfg) (s : St) (e : Ev) (h : Good s ∧ (c.routerOn = true → s.warmBad = false)) :
    Good (step Fix c s e) ∧ (c.routerOn = true → (step Fix c s e).warmBad = false) := by
  obtain ⟨⟨hn, hr, hf, hw⟩, hwr⟩ := h
  have hrw := step_keeps_ranBad_warmBad Fix c s e ⟨hr, hwr⟩
  refine ⟨⟨?_, hrw.1, ?_, ?_⟩, hrw.2⟩
  · -- P1: nothing sets nameFail in the fix
    cases e with
    | start daily kw a =>
        simp only [step]
        split
        · cases hv : Fix.route kw a with
          | none => obtain ⟨ts, hts, _⟩ := fixed_route_keeps_keywords kw a
                    exact absurd (hv.symm.trans hts) (by simp)
          | some ts => simp [hn]
        · simp [hn]
    | request => simp only [step]; split <;> simp [hn]
    | refresh => simp only [step]; split <;> simp [hn]
    | call n asks argsOk =>
        simp only [step]
        split
        · exact hn
        · cases n with
          | blank => simp [Fix, hn]
          | _ => simp only; split <;> (try split) <;> simp [hn]
    | summary n => simp only [step]; repeat' split
                   all_goals simp_all [Fix]
    | warm => simp [step, hn]
  · -- P4: the fixed router never fails a routed turn
    cases e with
    | start daily kw a =>
        simp only [step]
        split
        · cases hv : Fix.route kw a with
          | none => obtain ⟨ts, hts, _⟩ := fixed_route_keeps_keywords kw a
                    exact absurd (hv.symm.trans hts) (by simp)
          | some ts => simp [hf]
        · simp [hf]
    | request => simp only [step]; split <;> simp [hf]
    | refresh => simp only [step]; split <;> simp [hf]
    | call n asks argsOk =>
        simp only [step]
        split
        · exact hf
        · cases n with
          | blank => simp [Fix, hf]
          | _ => simp only; split <;> (try split) <;> simp [hf]
    | summary n => simp only [step]; repeat' split
                   all_goals simp_all [Fix]
    | warm => simp [step, hf]
  · -- the warm-up ghost
    cases e with
    | warm =>
        cases hon : c.routerOn
        · simp [step, hon, hw]
        · exact hrw.2 hon
    | start daily kw a =>
        simp only [step]
        split
        · cases Fix.route kw a <;> simp [hw]
        · simp [hw]
    | request => simp only [step]; split <;> simp [hw]
    | refresh => simp only [step]; split <;> simp [hw]
    | call n asks argsOk =>
        simp only [step]
        split
        · exact hw
        · cases n with
          | blank => simp [Fix, hw]
          | _ => simp only; split <;> (try split) <;> simp [hw]
    | summary n => simp only [step]; repeat' split
                   all_goals simp_all [Fix]

theorem fixed_run (c : Cfg) (evs : List Ev) :
    ∀ s : St, Good s ∧ (c.routerOn = true → s.warmBad = false) →
      Good (evs.foldl (step Fix c) s) ∧ (c.routerOn = true → (evs.foldl (step Fix c) s).warmBad = false) := by
  induction evs with
  | nil => intro s h; exact h
  | cons e rest ih => intro s h; exact ih _ (fixed_step c s e h)

/-- At a35b043, for every trace and every Mac: no toolset's tool ran unoffered and unopened, and the warm-up
opened nothing while routed (P2, P3 were right from the start). -/
theorem current_p2_p3_invariant (c : Cfg) (evs : List Ev) :
    (run Cur c evs).ranBad = false ∧ (c.routerOn = true → (run Cur c evs).warmBad = false) := by
  have key : ∀ (evs : List Ev) (s : St), s.ranBad = false ∧ (c.routerOn = true → s.warmBad = false) →
      (evs.foldl (step Cur c) s).ranBad = false ∧
      (c.routerOn = true → (evs.foldl (step Cur c) s).warmBad = false) := by
    intro evs
    induction evs with
    | nil => intro s h; exact h
    | cons e rest ih => intro s h; exact ih _ (step_keeps_ranBad_warmBad Cur c s e h)
  exact key evs {} ⟨rfl, fun _ => rfl⟩

/-- The fix, for every Mac and every trace of turns, calls, requests, refreshes, summaries and warm-ups:
(P1) no turn fails because of a call's name; (P2) a toolset's tool runs only up and offered or opened by its call;
(P4) a routed turn never loses its keyword toolsets, the router keeps them for every answer, and a keyword-only
toolset is exactly what the words said; (P3) each turn's open sets come from its own inputs, and the warm-up opens
nothing while routed; (P5) a request's instructions name only what it offers, in every turn but a rundown. -/
theorem fixed_invariant :
    (∀ (c : Cfg) (evs : List Ev),
      (run Fix c evs).nameFail = false ∧ (run Fix c evs).ranBad = false ∧ (run Fix c evs).routeFail = false ∧
      (c.routerOn = true → (run Fix c evs).warmBad = false)) ∧
    (∀ (kw : KSet) (a : Answer), ∃ ts, Fix.routeTurn kw a = some ts ∧ ∀ k, kw k = true → ts k = true) ∧
    (∀ (kw : KSet) (a : Answer) (k : Kit), k.keywordOnly = true →
      ∃ ts, Fix.routeTurn kw a = some ts ∧ ts k = kw k) ∧
    (∀ (c : Cfg) (s s' : St) (daily : Bool) (kw : KSet) (a : Answer),
      (step Fix c s (.start daily kw a)).cur = (step Fix c s' (.start daily kw a)).cur) ∧
    (∀ (c : Cfg) (m : Mode) (o : Open) (n : Name), m ≠ .daily →
      (∀ t, c.metas t = true → c.has .workspace t = true) →
      (∀ t, c.web t = true → c.inBase t = false ∧ c.body t = false) →
      named Fix c m o n = true → offers c m o n = true) := by
  refine ⟨?_, fixed_route_keeps_keywords, fixed_route_keyword_only, ?_, fixed_prompt_offers⟩
  · intro c evs
    obtain ⟨⟨hn, hr, hf, _⟩, hw⟩ := fixed_run c evs {} ⟨⟨rfl, rfl, rfl, rfl⟩, fun _ => rfl⟩
    exact ⟨hn, hr, hf, hw⟩
  · intro c s s' daily kw a
    exact (start_forgets Fix c s s' daily kw a).1

end Eve.RoutedTurn
