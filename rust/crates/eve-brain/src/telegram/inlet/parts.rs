//! A port of `telegram.py`, lines 5571-5852: a turn's parts, tool lists, Work-session tools, lent toolsets, warm-up.

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::telegram::*;

use indexmap::IndexSet;

/// A tool schema's `name` as `str(t["name"])` reads it.
fn tool_name(t: &Value) -> String {
    match t.get("name") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// Tool schemas (JSON values) as the objects the request takes.
fn objs(values: impl IntoIterator<Item = Value>) -> Vec<Obj> {
    values.into_iter().filter_map(|t| match t {
        Value::Object(o) => Some(o),
        _ => None,
    }).collect()
}

/// A watcher's `tools()` (a JSON array) as a list.
fn listed(value: Value) -> Vec<Value> {
    match value {
        Value::Array(a) => a,
        _ => Vec::new(),
    }
}

/// `parts[key]` (a list of tool objects).
fn part_objs(parts: &Obj, key: &str) -> Vec<Obj> {
    parts.get(key).and_then(Value::as_array).map(|a| a.iter().filter_map(|t| t.as_object().cloned()).collect()).unwrap_or_default()
}

impl TelegramInlet {
    /// py: telegram.py:5571 `async def _turn_parts(self, tail: int, *, warm: bool = False) -> tuple[dict[str, Any], set[str], list[dict[str, Any]]]:`
    ///
    /// What every round of a turn sends besides its items: request()'s keyword arguments, the tool names the
    /// model may call, and the app tools (the rundown narrows them). The cache warm-up (``warm``) sends the same,
    /// but with the router on it opens no toolset: the previous turn's would be stale (2026-10-05).
    ///
    /// Rust: the names are a plain list; AllowedNames' ``owns`` claim is applied where a response is parsed
    /// (``_think``), from the same rule (``_routed_owns`` routed, ``AppsBridge.owns`` otherwise).
    pub async fn turn_parts(self: &Arc<Self>, tail: i64, warm: bool) -> (Obj, Vec<String>, Vec<Obj>) {
        let mem_tools = self.memory_tools();
        let (daily, st_routed, direct_open, lent) = {
            let st = self.st.lock();
            (st.daily, st.routed, st.direct_open.clone(), st.lent.clone())
        };
        let (prof, today) = if self.memory.is_some() {
            let me = self.clone();
            let history = !(daily && !warm);
            tokio::task::spawn_blocking(move || me.prompt_memory(tail, history)).await.unwrap_or_default()
        } else {
            (String::new(), String::new())
        };
        let routed = if warm { self.toolset_mode != "off" } else { st_routed };
        // A rundown strips Share's tools (rundown.tools), so its block would name tools it does not
        // offer (review, 2026-10-05; the owner's rule: no name in a prompt that is not offered).
        let lend_blocks = !routed && !(daily && !warm);
        let (opened, lent): (Vec<String>, Vec<String>) = if warm && routed {
            (Vec::new(), Vec::new())
        } else {
            (sorted(direct_open), sorted(lent))
        };
        let lists = self.tool_lists(&opened, &lent, routed);
        let app_tools = part_objs(&lists, "app_tools");
        let watch_tools = part_objs(&lists, "watch_tools");
        let live = self.live_apps(); // eve's apps with a hub here, in tool order (app_registry)
        // a direct app's tool that this turn did not open is still the bridge's to answer (AllowedNames); with the
        // router on, so is a Composio tool while the workspace is closed (_tool opens it and runs the call)
        let allowed: Vec<String> = lists
            .get("allowed")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|n| n.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        // a rundown offers only its read tools (rundown.tools), so events_search's line and the tool-choice lines
        // ride every other turn; each line names only tools this turn offers (intent_block)
        let everyday = !(daily && !warm);
        let env = self.environ.as_ref();
        let choice = if everyday && eve_menu_tools::enabled(env) {
            let mut names: IndexSet<String> = IndexSet::new();
            for t in watch_tools.iter().chain(mem_tools.iter()).chain(app_tools.iter()) {
                names.insert(tool_name(&Value::Object(t.clone())));
            }
            names.extend(allowed.iter().cloned());
            for t in own_tools() {
                names.insert(tool_name(&Value::Object(t)));
            }
            names.insert(eve_web::websearch::TOOL_NAME.to_string());
            let names: Vec<String> = names.into_iter().collect();
            intent_block(&names, &self.direct_up())
        } else {
            String::new()
        };
        let direct_started = self.direct.as_ref().is_some_and(|d| d.started());
        let composio_note = if !routed
            && self.composio_up()
            && app_tools.iter().any(|t| t.get("name").and_then(Value::as_str).unwrap_or("").starts_with("COMPOSIO_"))
        {
            // which account each app call names, while an app has two; only while Composio's tools ride: a failed
            // start still knows the accounts (Lean RoutedTurn P5, 2026-10-05)
            self.apps.as_ref().map(|a| a.accounts_note()).unwrap_or_default()
        } else {
            String::new()
        };
        let chief = self.chief.lock().clone();
        let menu_wide = eve_menu_tools::enabled(env);
        let mut blocks: Vec<String> = vec![
            choice,
            if everyday { self.events_line() } else { String::new() },
            self.watcher.as_ref().map(|w| w.instructions()).unwrap_or_default(),
            self.meeter.as_ref().map(|m| m.instructions().to_string()).unwrap_or_default(),
        ];
        blocks.extend(live.iter().filter_map(|(_, hub)| hub.as_ref().map(|h| h.instructions(&Obj::new()))));
        blocks.extend([
            self.canvas.as_ref().map(|c| c.instructions().to_string()).unwrap_or_default(),
            if self.agentcard.is_some() { eve_agentcard::AgentCard::instructions().to_string() } else { String::new() },
            self.america.as_ref().map(|a| a.instructions().to_string()).unwrap_or_default(),
            if self.opentable_enabled { eve_opentable::instructions(menu_wide) } else { String::new() },
            match &self.share {
                Some(share) if lend_blocks => share.instructions(),
                _ => String::new(),
            },
            chief.as_ref().map(|c| eve_chief::instructions(&c.live)).unwrap_or_default(),
            composio_note,
            // which direct apps are up and what may change in each
            if direct_started { self.direct.as_ref().map(|d| d.instructions()).unwrap_or_default() } else { String::new() },
        ]);
        let watch_block = blocks.into_iter().filter(|b| !b.is_empty()).collect::<Vec<_>>().join("\n\n");
        let chat_log = self.st.lock().chat_log.is_some();
        let mut parts = Obj::new();
        parts.insert("profile".into(), Value::String(prof));
        parts.insert("app_tools".into(), Value::Array(app_tools.iter().cloned().map(Value::Object).collect()));
        parts.insert("vault".into(), Value::Bool(self.vault.is_some()));
        parts.insert("today".into(), Value::String(today));
        parts.insert("memory_tools".into(), Value::Array(mem_tools.into_iter().map(Value::Object).collect()));
        parts.insert("watch_tools".into(), Value::Array(watch_tools.into_iter().map(Value::Object).collect()));
        parts.insert("watch_block".into(), Value::String(watch_block));
        parts.insert("direct_apps".into(), json!(self.direct_up()));
        parts.insert("recall".into(), Value::String(recall_line(chat_log, self.memory.is_some())));
        parts.insert("chat_tools".into(), lists.get("chat_tools").cloned().unwrap_or_else(|| json!([])));
        if routed {
            parts.insert("lent_block".into(), Value::String(self.lent_block(&lent)));
        }
        (parts, allowed, app_tools)
    }

    /// py: telegram.py:5632 `def _tool_lists(self, opened: set[str], lent: set[str], routed: bool) -> dict[str, Any]:`
    ///
    /// The tool lists one turn offers, and their callable names: what ``_turn_parts`` sends and what a Work
    /// session's eve MCP server offers (``session_tools``), built in one place so the two cannot drift
    /// (docs/work-sessions.md). ``allowed`` is a plain set here; ``_turn_parts`` wraps it in AllowedNames.
    ///
    /// Rust: an object with the keys memory_tools, app_tools, watch_tools, allowed (a list of names, each once)
    /// and chat_tools.
    pub(crate) fn tool_lists(self: &Arc<Self>, opened: &[String], lent: &[String], routed: bool) -> Obj {
        let env = self.environ.as_ref();
        let mem_tools = self.memory_tools();
        let app_tools = self.app_tools(Some(opened), Some(lent), Some(routed));
        let loc_tools = eve_places::phone_location::tools(env); // once a turn: the phone's location, while it has shared one
        let mut sci_tools = eve_science_search::tools(env); // papers' titles and abstracts, while EVE_SCIENCE_SEARCH is on
        // The New York Times (nyt.py), while EVE_NYT is on: headlines always, the four key tools with NYT_API_KEY
        sci_tools.extend(eve_nyt::tools(env));
        // a real picture for a card (Wikipedia), while EVE_FIND_IMAGE is on: app turns only, the one place a card is
        // drawn; a call is spoken, so it never pays for the tool or the lookup (owner, 2026-10-13: voice "should be
        // faster")
        let pic_tools = if ctx().app.is_some() { eve_find_image::tools(env) } else { Vec::new() };
        let live = self.live_apps(); // eve's apps with a hub here, in tool order (app_registry)
        let chief = self.chief.lock().clone();
        let chat_log = self.st.lock().chat_log.is_some();
        let menu = self.menu_tools();
        let mut allowed: IndexSet<String> = IndexSet::new();
        allowed.extend(app_tools.iter().map(|t| tool_name(&Value::Object(t.clone()))));
        allowed.extend(mem_tools.iter().map(|t| tool_name(&Value::Object(t.clone()))));
        if self.vault.is_some() {
            allowed.extend(eve_second_brain::SECOND_BRAIN_TOOL_NAMES.iter().map(|n| n.to_string()));
        }
        if self.watcher.is_some() {
            allowed.extend(eve_web::watch::TOOL_NAMES.iter().map(|n| n.to_string()));
        }
        if self.meeter.is_some() {
            allowed.extend(eve_meet::TOOL_NAMES.iter().map(|n| n.to_string()));
        }
        for (app, _) in &live {
            if let Some(info) = app.module_info() {
                allowed.extend(info.tool_names().iter().cloned());
            }
        }
        allowed.extend(loc_tools.iter().map(tool_name));
        allowed.extend(sci_tools.iter().map(tool_name));
        allowed.extend(pic_tools.iter().map(tool_name));
        if self.canvas.is_some() {
            allowed.extend(eve_canvas::TOOL_NAMES.iter().map(|n| n.to_string()));
        }
        if self.agentcard.is_some() {
            allowed.extend(eve_agentcard::TOOL_NAMES.iter().map(|n| n.to_string()));
        }
        if self.america.is_some() {
            allowed.extend(eve_america::TOOL_NAMES.iter().map(|n| n.to_string()));
        }
        if self.opentable_enabled {
            allowed.extend(eve_opentable::TOOL_NAMES.iter().map(|n| n.to_string()));
        }
        if self.share.is_some() {
            allowed.extend(crate::share::TOOL_NAMES.iter().map(|n| n.to_string()));
        }
        if chief.is_some() {
            allowed.extend(eve_chief::TOOL_NAMES.iter().map(|n| n.to_string()));
        }
        if chat_log {
            allowed.insert(tool_name(&RECENT_MESSAGES_TOOL));
        }
        allowed.extend(menu.iter().map(|t| tool_name(&Value::Object(t.clone()))));
        // Optional shared chat/call services contribute their tools and instructions here:
        // all ride one slot (the chief's enums and block name only its live steps and checks, chief.tools)
        let mut watch_tools: Vec<Obj> = menu;
        if let Some(w) = &self.watcher {
            watch_tools.extend(objs(listed(w.tools())));
        }
        if let Some(m) = &self.meeter {
            watch_tools.extend(objs(m.tools()));
        }
        for (_, hub) in &live {
            if let Some(hub) = hub {
                watch_tools.extend(objs(hub.tools()));
            }
        }
        watch_tools.extend(objs(loc_tools));
        watch_tools.extend(objs(sci_tools));
        watch_tools.extend(objs(pic_tools));
        if let Some(c) = &self.canvas {
            watch_tools.extend(objs(c.tools()));
        }
        if let Some(a) = &self.agentcard {
            watch_tools.extend(objs(a.tools()));
        }
        if let Some(a) = &self.america {
            watch_tools.extend(objs(a.tools()));
        }
        if self.opentable_enabled {
            watch_tools.extend(objs(eve_opentable::tools(eve_menu_tools::enabled(env))));
        }
        // routed: Share's tools are a lent toolset, last in the list (_app_tools)
        if let Some(share) = &self.share {
            if !routed {
                watch_tools.extend(share.tools());
            }
        }
        if let Some(chief) = &chief {
            watch_tools.extend(objs(eve_chief::tools_in(&chief.live, env)));
        }
        let mut out = Obj::new();
        out.insert("memory_tools".into(), Value::Array(mem_tools.into_iter().map(Value::Object).collect()));
        out.insert("app_tools".into(), Value::Array(app_tools.into_iter().map(Value::Object).collect()));
        out.insert("watch_tools".into(), Value::Array(watch_tools.into_iter().map(Value::Object).collect()));
        out.insert("allowed".into(), json!(allowed.into_iter().collect::<Vec<_>>()));
        out.insert("chat_tools".into(), if chat_log { json!([RECENT_MESSAGES_TOOL.clone()]) } else { json!([]) });
        out
    }

    /// py: telegram.py:5682 `def session_tools(self) -> tuple[list[dict[str, Any]], set[str]]:`
    ///
    /// The tools a Work session's eve MCP server offers, and their names: the chat brain's own lists
    /// (``_tool_lists``) as an unrouted turn with every up direct app opened offers them, less SESSION_HIDDEN and
    /// any tool that is not a plain function (the hosted web search has no MCP form). Never a second copy: a
    /// tool that comes or goes for the chat comes or goes here on the next tools/list.
    pub fn session_tools(self: &Arc<Self>) -> (Vec<Obj>, Vec<String>) {
        let lists = self.tool_lists(&self.direct_up(), &[], false);
        let offered = tools_for(
            &self.config,
            &part_objs(&lists, "memory_tools"),
            &part_objs(&lists, "app_tools"),
            self.vault.is_some(),
            &part_objs(&lists, "watch_tools"),
            &part_objs(&lists, "chat_tools"),
        );
        let mut out = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        for t in offered {
            let Some(name) = t.get("name").and_then(Value::as_str).map(str::to_string) else {
                continue;
            };
            if t.get("type").and_then(Value::as_str) != Some("function") || name.is_empty() {
                continue;
            }
            if SESSION_HIDDEN.contains(&name.as_str()) || seen.contains(&name) {
                continue;
            }
            seen.push(name);
            out.push(t);
        }
        (out, seen)
    }

    /// py: telegram.py:5702 `async def session_call(self, name: str, args: Any, *, sid: str) -> tuple[dict[str, Any], str]:`
    ///
    /// One tool call from a Work session → (its result, the outside source it read, or ""). The call takes the
    /// chat's own path: a name the session was not offered is refused; ``tool_gate.needs_confirmation`` with
    /// SESSION_READ as the read (so recording, joining and answering ask, as after any read), then ``_gate``,
    /// which asks the owner on the phone and waits (``_ask_user``, ``config.ask_timeout_secs``); then ``_tool``,
    /// where the apps' own policies ask for their consequential writes, purchases and shares as in the chat.
    /// A question nobody answers ends as OWNER_NO_ANSWER. The chat turn's ``_taint`` is never read or written
    /// here. Known side effect: a direct app the call names is opened for the chat's next turn too
    /// (``_direct_tool``'s ensure_open), as a call from the chat would.
    pub async fn session_call(self: &Arc<Self>, name: &str, args: &Value, sid: &str) -> (Obj, String) {
        let refuse = |reason: &str| {
            let mut r = Obj::new();
            r.insert("ok".into(), Value::Bool(false));
            r.insert("reason".into(), Value::String(reason.to_string()));
            r
        };
        let Some(chat_id) = self.st.lock().chat_id else {
            return (refuse(SESSION_NO_CHAT), String::new());
        };
        let short: String = sid.chars().take(8).collect();
        let (_, allowed) = self.session_tools();
        if !allowed.iter().any(|n| n == name) {
            tracing::info!("telegram: work session {}: refused a name it was not offered", short);
            return (refuse(SESSION_NOT_OFFERED), String::new());
        }
        let args: Obj = args.as_object().cloned().unwrap_or_default();
        let session = Arc::new(Mutex::new(WorkSession { sid: sid.to_string(), unanswered: false }));
        let mut scope = ctx();
        scope.session = Some(session.clone());
        let mut result = in_ctx(scope, async {
            if eve_tool_gate::needs_confirmation(name, &args, SESSION_READ) {
                match self.gate(name, &args, chat_id, Some(SESSION_READ)).await {
                    Some(r) if !r.is_empty() => r,
                    _ => self.tool(name, &args, chat_id).await,
                }
            } else {
                self.tool(name, &args, chat_id).await
            }
        })
        .await;
        if session.lock().unanswered && result.get("ok") == Some(&Value::Bool(false)) {
            result = refuse(OWNER_NO_ANSWER);
        }
        let outside = if eve_tool_gate::taints(name, &args) { eve_tool_gate::source(name).unwrap_or_default() } else { String::new() };
        let ok = result.get("ok").map(|v| crate::telegram::inlet::turn::truthy(Some(v))).unwrap_or(true);
        tracing::info!("telegram: work session {}: {} {}", short, name, if ok { "ok" } else { "not ok" });
        (result, outside)
    }

    /// py: telegram.py:5738 `def _menu_tools(self) -> list[dict[str, Any]]:`
    ///
    /// The / menu's tools (menu_tools.py), then events_search while Ticketmaster's key is set: on every turn,
    /// in a fixed order, so the cached prefix holds. None while EVE_MENU_TOOLS is off.
    pub(crate) fn menu_tools(self: &Arc<Self>) -> Vec<Obj> {
        let env = self.environ.as_ref();
        if !eve_menu_tools::enabled(env) {
            return Vec::new();
        }
        let mut tools = objs(eve_menu_tools::tools(env));
        tools.extend(objs(self.events.tools()));
        tools
    }

    /// py: telegram.py:5745 `def _events_line(self) -> str:`
    ///
    /// events_search's instruction while it is lent; it names watch_add's ticketmaster kind only when the
    /// watcher offers it (no name in a prompt that the turn does not offer).
    pub(crate) fn events_line(self: &Arc<Self>) -> String {
        if !eve_menu_tools::enabled(self.environ.as_ref()) || !self.events.enabled() {
            return String::new();
        }
        let watching = self.watcher.as_ref().is_some_and(|w| w.kinds().contains(&"ticketmaster"));
        eve_events::instructions(watching)
    }

    /// py: telegram.py:5754 `def _lendable(self) -> list[str]:`
    ///
    /// The lent toolsets this Mac has (in LENT_ORDER): Share when lent, the workspace while
    /// Composio's session is up.
    pub(crate) fn lendable(self: &Arc<Self>) -> Vec<String> {
        let share = self.share.is_some();
        let workspace = self.composio_up();
        LENT_ORDER
            .iter()
            .filter(|name| match **name {
                eve_toolset_router::SHARE => share,
                eve_toolset_router::WORKSPACE => workspace,
                _ => false,
            })
            .map(|n| n.to_string())
            .collect()
    }

    /// py: telegram.py:5761 `def _lent_block(self, lent: Collection[str]) -> str:`
    ///
    /// The open lent toolsets' instructions, for request()'s lent_block; "" with none open.
    pub(crate) fn lent_block(self: &Arc<Self>, lent: &[String]) -> String {
        let mut blocks = Vec::new();
        if lent.iter().any(|n| n == eve_toolset_router::WORKSPACE) && self.composio_up() {
            blocks.push(self.apps.as_ref().map(|a| a.accounts_note()).unwrap_or_default());
        }
        if lent.iter().any(|n| n == eve_toolset_router::SHARE) {
            if let Some(share) = &self.share {
                blocks.push(share.instructions());
            }
        }
        blocks.into_iter().filter(|b| !b.is_empty()).collect::<Vec<_>>().join("\n\n")
    }

    /// py: telegram.py:5770 `def _refresh_lent(self, parts: dict[str, Any], allowed: set[str]) -> list[dict[str, Any]]:`
    ///
    /// After this turn's toolsets changed: the app tools (and, routed, the lent instructions) rebuilt into
    /// ``parts`` and ``allowed``, in place. Returns the new app tools.
    ///
    /// Rust: `parts` and `allowed` are `&mut`, as Python changes them in place.
    pub(crate) fn refresh_lent(self: &Arc<Self>, parts: &mut Obj, allowed: &mut Vec<String>) -> Vec<Obj> {
        let app_tools = self.app_tools(None, None, None);
        parts.insert("app_tools".into(), Value::Array(app_tools.iter().cloned().map(Value::Object).collect()));
        let (routed, lent) = {
            let st = self.st.lock();
            (st.routed, sorted(st.lent.clone()))
        };
        if routed {
            parts.insert("lent_block".into(), Value::String(self.lent_block(&lent)));
        }
        for t in &app_tools {
            let name = tool_name(&Value::Object(t.clone()));
            if !allowed.contains(&name) {
                allowed.push(name);
            }
        }
        app_tools
    }

    /// py: telegram.py:5780 `def _routed_owns(self, name: str) -> bool:`
    ///
    /// AllowedNames' claim while routed: a direct app's tool (AppsBridge.owns), or Composio's while it is up.
    pub(crate) fn routed_owns(self: &Arc<Self>, name: &str) -> bool {
        if self.direct.as_ref().is_some_and(|d| d.owns(name)) {
            return true;
        }
        self.composio_up() && self.apps.as_ref().is_some_and(|a| a.names().contains(name))
    }

    /// py: telegram.py:5786 `def _lend(self, name: str) -> None:`
    ///
    /// A call to a closed lent toolset's tool opens it for the rest of the turn (as AppsBridge.ensure_open for
    /// a direct app): the call runs now, and its tools are offered from the next round. Never a failed turn.
    pub(crate) fn lend(self: &Arc<Self>, name: &str) {
        let routed = self.st.lock().routed;
        if routed && self.lendable().iter().any(|n| n == name) {
            self.st.lock().lent.insert(name.to_string());
        }
    }

    /// py: telegram.py:5792 `def _lent_tools(self, name: str) -> list[dict[str, Any]]:`
    pub(crate) fn lent_tools(self: &Arc<Self>, name: &str) -> Vec<Obj> {
        if name == eve_toolset_router::SHARE {
            if let Some(share) = &self.share {
                return share.tools();
            }
        }
        if name == eve_toolset_router::WORKSPACE && self.composio_up() {
            if let Some(apps) = &self.apps {
                return apps.tools();
            }
        }
        Vec::new()
    }

    /// py: telegram.py:5802 `def _open_toolsets(self, args: dict[str, Any]) -> dict[str, Any]:`
    ///
    /// open_app while routed: opens direct apps and lent toolsets alike, for the rest of the turn. A name that
    /// is no toolset here is not repeated back (the owner's rule): the result names the ones that are.
    pub fn open_toolsets(self: &Arc<Self>, args: &Obj) -> Obj {
        let (up, lendable) = (self.direct_up(), self.lendable());
        let here: Vec<String> = up.iter().chain(lendable.iter()).cloned().collect();
        let wanted = eve_apps_mcp::open_asked(args);
        let good: Vec<String> = wanted.iter().filter(|a| here.contains(a)).cloned().collect();
        let mut out = Obj::new();
        if good.is_empty() {
            out.insert("ok".into(), Value::Bool(false));
            out.insert("reason".into(), Value::String(format!("that is not one of the toolsets here; the ones here: {}", here.join(", "))));
            return out;
        }
        {
            let mut st = self.st.lock();
            st.direct_open.extend(good.iter().filter(|a| up.contains(a)).cloned());
            st.lent.extend(good.iter().filter(|a| lendable.contains(a)).cloned());
        }
        let mut tools: Vec<String> = Vec::new();
        if !up.is_empty() {
            if let Some(direct) = &self.direct {
                for t in direct.tools() {
                    let name = tool_name(&t);
                    if good.iter().any(|g| g == eve_apps_mcp::split_name(&name).0) {
                        tools.push(name);
                    }
                }
            }
        }
        for name in &good {
            if lendable.contains(name) {
                tools.extend(self.lent_tools(name).into_iter().map(|t| tool_name(&Value::Object(t))));
            }
        }
        out.insert("ok".into(), Value::Bool(true));
        out.insert("opened".into(), json!(good));
        out.insert("tools".into(), json!(tools));
        if good.len() < wanted.len() {
            out.insert("note".into(), Value::String(format!("some names asked for are not toolsets here; the ones here: {}", here.join(", "))));
        }
        out
    }

    /// py: telegram.py:5823 `def _previous_said(self) -> str:`
    ///
    /// The owner's message before this turn's (a follow-up's context for the router). _answer_turn notes the
    /// running turn's own line before _think, so that line is passed over; "" with none.
    pub(crate) fn previous_said(self: &Arc<Self>) -> String {
        let mut said: Vec<String> = self.current_turns().into_iter().filter(|(who, _)| who == "user").map(|(_, t)| t).collect();
        let now = self.st.lock().turn_said.clone();
        if let Some(last) = said.last() {
            if *last == now || last.starts_with(&format!("{now} [")) {
                said.pop();
            }
        }
        said.pop().unwrap_or_default()
    }

    /// py: telegram.py:5832 `async def _warm(self) -> None:`
    ///
    /// A call opened: send the chat's own prompt once with nothing to answer, so the owner's first question
    /// finds it cached (measured 2026-09-25: a call's first turn read 12k tokens with none cached). The reply is
    /// thrown away, and nothing is recorded but its spend.
    pub async fn warm(self: &Arc<Self>) {
        let tail = {
            let st = self.st.lock();
            let from = st.turn_kinds.len().saturating_sub(HISTORY_TURNS);
            Self::shown(&st.turn_kinds[from..])
        };
        let mut history = self.history(); // first: it sets the log window the today block counts against
        let (parts, _, _) = self.turn_parts(tail, true).await;
        let call_note = if crate::telegram::prompts::lite(self.environ.as_ref()) { CALL_NOTE_LITE } else { CALL_NOTE };
        history.push(message_item("user", "(the call connected)"));
        let context = format!("{}\n\n{}", turn_context(&(self.brief)(), None), call_note);
        let mut payload = self.request_with(&history, &context, "", &parts);
        payload.insert("max_output_tokens".into(), json!(16));
        payload.insert("reasoning".into(), json!({"effort": "low"}));
        match (self.create)(Value::Object(payload.clone())).await {
            Ok(response) => {
                let model = match payload.get("model") {
                    Some(Value::String(m)) => m.clone(),
                    None | Some(Value::Null) => String::new(),
                    Some(other) => other.to_string(),
                };
                eve_spend::record_response(eve_spend::CALLS, &response, &model, "openrouter", None);
            }
            // a cold first answer, nothing worse
            Err(e) => tracing::info!("telegram: the call's cache warm-up failed ({})", crate::telegram::inlet::turn::error_type(&e)),
        }
    }
}

/// A set of names in one fixed order (Python's sets have none the code relies on).
fn sorted(names: HashSet<String>) -> Vec<String> {
    let mut v: Vec<String> = names.into_iter().collect();
    v.sort();
    v
}
