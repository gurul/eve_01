//! A port of `telegram.py`, lines 4774-5103: the chat's record: notes, the chat log, history, the memory prompt.

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::telegram::*;

use eve_chat_log::Line as LogLine;

/// One chat log line as the Python object reads (`dataclasses.asdict(line)`): what `_recall_seen` and
/// `quoted_line` hold, in the field order the log writes.
pub(crate) fn chat_line_value(ln: &LogLine) -> Value {
    let mut m = Obj::new();
    m.insert("seq".into(), json!(ln.seq));
    m.insert("t".into(), json!(ln.t));
    m.insert("chat".into(), json!(ln.chat));
    m.insert("who".into(), json!(ln.who));
    m.insert("text".into(), json!(ln.text));
    m.insert("kind".into(), json!(ln.kind));
    m.insert("conv".into(), json!(ln.conv));
    m.insert("boot".into(), json!(ln.boot));
    m.insert("key".into(), json!(ln.key));
    m.insert("quotes".into(), json!(ln.quotes));
    m.insert("outside".into(), json!(ln.outside));
    m.insert("extra".into(), Value::Object(ln.extra.clone()));
    Value::Object(m)
}

/// A `list[dict[str, str]]` (an `att` or a `src`) as the JSON the transcript writes. The transcript picks and
/// orders the keys it keeps itself (id, name, kind; kind, text, when), so a map's own order is never what lands.
fn str_maps(rows: &[HashMap<String, String>]) -> Vec<Value> {
    rows.iter()
        .map(|row| {
            let mut keys: Vec<&String> = row.keys().collect();
            keys.sort();
            let mut m = Obj::new();
            for k in keys {
                m.insert(k.clone(), Value::String(row[k].clone()));
            }
            Value::Object(m)
        })
        .collect()
}

/// `text[:n]`, in characters.
fn head_chars(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

impl TelegramInlet {
    /// `inlet._conv`: the transcript conversation this chat is in, None between conversations.
    pub fn conversation(&self) -> Option<String> {
        self.st.lock().conv.clone()
    }

    /// `inlet._turn_kinds`: each history line's transcript kind ("" for a line the transcript has not).
    pub fn turn_kinds(&self) -> Vec<String> {
        self.st.lock().turn_kinds.clone()
    }

    /// py: telegram.py:4774 `def _note(self, who: str, text: str, kind: str = "say", *, said: Optional[str] = None, at: Optional[float] = None, att: Optional[list[dict[str, str]]] = None, outside: bool = False, src: Optional[list[dict[str, str]]] = None) -> None:`
    ///
    /// One line of this chat: into the transcript as `kind` (say, command, relay, image), and, for a line
    /// said to eve, into the model's history. `said` is the transcript's words when they differ from the
    /// history's (an image turn). ``outside``: eve wrote it from words it did not write (a tainted turn's
    /// reply, a task's closing sentence), so a later turn whose prompt still shows it starts tainted
    /// (tool_gate.MARKS, carried). ``src``: the memory lines a reply leaned on, for the transcript line alone
    /// (memory_sources.py); the history and the chat log never carry them.
    ///
    /// Relay and command lines are the transcript's only (HISTORY_HIDDEN_KINDS). Live, 2026-09-24 08:44: the
    /// history the brain was shown was eight lines the owner had typed to Claude Code and code words ("The
    /// expense spllitter doesn't work", "Ultrathink", "Claude off"), with none of Claude's replies, and
    /// think_hard built the owner's "plan" out of them. The today block leaves those kinds out as well, so
    /// neither the history nor the prompt ever shows words that were not said to eve.
    #[allow(clippy::too_many_arguments)]
    pub fn note(self: &Arc<Self>, who: &str, text: &str, kind: &str, said: Option<&str>, at: Option<f64>, att: Option<&[HashMap<String, String>]>, outside: bool, src: Option<&[HashMap<String, String>]>) {
        let words = said.unwrap_or(text);
        if HISTORY_HIDDEN_KINDS.contains(&kind) {
            self.record(who, kind, words, "", att, false, None);
            self.st.lock().last_turn_at = Some((self.clock)());
            if kind == "relay" {
                // with the chat log, the relay is one note in the history, never the words as said to eve
                let label = if who == "user" { "The owner typed to Claude Code: " } else { "Claude Code asked: " };
                let note = format!("{label}{}", eve_chat_log::note_text(text, eve_chat_log::NOTE_CHARS));
                self.event(&note, "", who != "user");
            }
            return;
        }
        if outside {
            // as the history shows it to an app turn (whole) and to every other turn (one plain line)
            eve_tool_gate::MARKS.add([text.to_string(), history_text(text, false)]);
        }
        self.st.lock().turns.push((who.to_string(), text.to_string()));
        let written = self.record(who, kind, words, "", att, outside, src);
        let (conv, quotes) = {
            let mut st = self.st.lock();
            st.turn_kinds.push(if written { kind.to_string() } else { String::new() });
            st.last_turn_at = Some((self.clock)());
            let quoted = if who == "eve" && !st.recall_seen.is_empty() { quoted_line(text, &st.recall_seen) } else { None };
            let quotes = quoted.as_ref().and_then(|q| q.get("seq")).and_then(Value::as_i64).unwrap_or(0);
            let conv = if written { st.conv.clone().unwrap_or_default() } else { String::new() };
            (conv, quotes)
        };
        self.log_line(who, text, if written { kind } else { "" }, Some(&conv), "", at, quotes, outside);
    }

    /// py: telegram.py:4808 `def _history_only(self, who: str, text: str) -> None:`
    ///
    /// A line for the history alone (a receipt's "(Starred.)"): the transcript already has it as a tool line.
    pub(crate) fn history_only(self: &Arc<Self>, who: &str, text: &str) {
        {
            let mut st = self.st.lock();
            st.turns.push((who.to_string(), text.to_string()));
            st.turn_kinds.push(String::new());
            st.last_turn_at = Some((self.clock)());
        }
        self.log_line(who, text, "", Some(""), "", None, 0, false);
    }

    /// py: telegram.py:4816 `def _log_chat(self) -> int:`
    pub(crate) fn log_chat(self: &Arc<Self>) -> i64 {
        self.st.lock().chat_id.unwrap_or(0)
    }

    /// The chat log, when one is kept.
    fn chat_log_now(self: &Arc<Self>) -> Option<Arc<eve_chat_log::ChatLog>> {
        self.st.lock().chat_log.clone()
    }

    /// py: telegram.py:4819 `def _log_line(self, who: str, text: str, kind: str = "", conv: Optional[str] = "", *, key: str = "", at: Optional[float] = None, quotes: int = 0, outside: bool = False) -> None:`
    ///
    /// One line into the chat log (nothing without one), and the RAM turns synced to it. Never raises.
    /// ``outside``: kept as an outside line (chat_log.Line.outside), so a restart cannot launder it.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn log_line(self: &Arc<Self>, who: &str, text: &str, kind: &str, conv: Option<&str>, key: &str, at: Option<f64>, quotes: i64, outside: bool) {
        let Some(log) = self.chat_log_now() else {
            return;
        };
        let at = at.unwrap_or_else(|| (self.wall)());
        // the log never costs the chat: it refuses quietly (an unknown speaker, empty words) and never raises
        log.append_with(
            self.log_chat(),
            who,
            text,
            eve_chat_log::Append {
                kind: kind.to_string(),
                conv: conv.unwrap_or("").to_string(),
                key: key.to_string(),
                at: Some(at),
                quotes,
                outside,
            },
        );
        self.sync_turns();
    }

    /// py: telegram.py:4833 `def _seed_log(self) -> None:`
    ///
    /// Once per chat: with memory on, the chat's earlier lines from the transcripts into the log
    /// (chat_log.ChatLog.seed), so an install whose log is newer than its transcripts recalls what came before
    /// the log's first boot. The log marks the chat in its file; this set only saves the check. Never raises.
    pub(crate) fn seed_log(self: &Arc<Self>) {
        let chat = self.log_chat();
        let (Some(log), Some(memory)) = (self.chat_log_now(), self.memory.clone()) else {
            return;
        };
        {
            let mut st = self.st.lock();
            if st.seeded_chats.contains(&chat) {
                return;
            }
            st.seeded_chats.insert(chat);
        }
        let store = memory.transcripts();
        if log.seed(chat, Some(store.as_ref() as &dyn eve_chat_log::SeedStore)) > 0 {
            self.sync_turns();
        }
    }

    /// py: telegram.py:4847 `def _event(self, text: str, *, key: str = "", agent: bool = False) -> None:`
    ///
    /// A hand-off note: one short line saying what was handed to an agent, or what came back. Shown in the
    /// history as a bracketed developer note, so "did you tell the agent?" has something to answer from. The
    /// caller caps anything an agent or a page wrote (chat_log.note_text). Only with the chat log.
    ///
    /// ``agent``: the words after the note's fixed label ("Codex answered: …") are an agent's, which can quote a
    /// page or a file. Those are never sent at developer level: the history shows the label alone
    /// (AGENT_NOTE), and recent_messages returns the words as a tool result, which the instructions call
    /// information, never instructions (review, 2026-10-08). The label must hold no ": ".
    pub(crate) fn event(self: &Arc<Self>, text: &str, key: &str, agent: bool) {
        if self.chat_log_now().is_some() {
            let capped = head_chars(text, eve_chat_log::NOTE_CHARS + 60);
            self.log_line("event", &capped, if agent { AGENT_KIND } else { "" }, Some(""), key, None, 0, false);
        }
    }

    /// py: telegram.py:4859 `def _sync_turns(self) -> None:`
    ///
    /// With the chat log, the RAM turns are the log's owner and eve lines for this chat (what
    /// _previous_said and the link carry-over read). A forget in the log reaches them here.
    pub(crate) fn sync_turns(self: &Arc<Self>) {
        let Some(log) = self.chat_log_now() else {
            return;
        };
        let lines = log.lines(self.log_chat());
        let turns: Vec<(String, String)> = lines
            .iter()
            .filter(|ln| ln.who == "user" || ln.who == "eve")
            .map(|ln| (ln.who.clone(), ln.text.clone()))
            .collect();
        self.st.lock().turns = turns;
        let outside: Vec<String> = lines.iter().filter(|ln| ln.outside).map(|ln| ln.text.clone()).collect();
        if !outside.is_empty() {
            // as _note marks them: whole (an app turn's history) and as one plain line (every other turn's)
            let plain: Vec<String> = outside.iter().map(|t| history_text(t, false)).collect();
            eve_tool_gate::MARKS.add(outside.iter().chain(plain.iter()));
        }
    }

    /// py: telegram.py:4874 `def _current_turns(self) -> list[tuple[str, str]]:`
    ///
    /// The owner's and eve's lines of the conversation going on now: with the chat log, the lines after its
    /// newest quiet spell (idle_close_secs) or restart, so a link or a message from hours or days ago is never
    /// "that link" or the follow-up's context (review, 2026-10-08). Without it, the RAM turns, which a quiet
    /// spell already clears.
    pub(crate) fn current_turns(self: &Arc<Self>) -> Vec<(String, String)> {
        let Some(log) = self.chat_log_now() else {
            return self.st.lock().turns.clone();
        };
        let lines = log.lines(self.log_chat());
        let mut start = 0;
        for i in (1..lines.len()).rev() {
            let (before, ln) = (&lines[i - 1], &lines[i]);
            if ln.t - before.t >= self.config.idle_close_secs
                || (!before.boot.is_empty() && !ln.boot.is_empty() && before.boot != ln.boot)
            {
                start = i;
                break;
            }
        }
        lines[start..]
            .iter()
            .filter(|ln| ln.who == "user" || ln.who == "eve")
            .map(|ln| (ln.who.clone(), ln.text.clone()))
            .collect()
    }

    /// py: telegram.py:4894 `def _log_window(self) -> tuple[list[Any], bool]:`
    ///
    /// (the log lines the history shows, whether older ones exist). The window starts at a remembered line
    /// and moves only when it outgrows CHAT_LOG_HISTORY_LINES or _CHARS, then in one step to the KEEP sizes.
    pub(crate) fn log_window(self: &Arc<Self>) -> (Vec<LogLine>, bool) {
        let Some(log) = self.chat_log_now() else {
            return (Vec::new(), false);
        };
        let lines = log.lines(self.log_chat());
        let generation = log.generation() as i64;
        let mut st = self.st.lock();
        if st.window_gen != generation {
            st.window_gen = generation;
            st.window_seq = 0;
        }
        let mut start = lines.iter().position(|ln| ln.seq >= st.window_seq).unwrap_or(lines.len());
        let mut window: Vec<LogLine> = lines[start..].to_vec();
        let chars = |w: &[LogLine]| w.iter().map(|ln| ln.text.chars().count()).sum::<usize>();
        if window.len() > CHAT_LOG_HISTORY_LINES || chars(&window) > CHAT_LOG_HISTORY_CHARS {
            let keep_from = window.len().saturating_sub(CHAT_LOG_HISTORY_KEEP);
            window.drain(..keep_from);
            while window.len() > 1 && chars(&window) > CHAT_LOG_HISTORY_KEEP_CHARS {
                window.remove(0);
            }
            start = lines.len() - window.len();
        }
        if let Some(first) = window.first() {
            st.window_seq = first.seq;
        }
        (window, start > 0)
    }

    /// py: telegram.py:4913 `def _gap(self, before: Any, t: float, boot: str) -> str:`
    pub(crate) fn gap(self: &Arc<Self>, before: &LogLine, t: f64, boot: &str) -> String {
        let secs = t - before.t;
        let restart = !before.boot.is_empty() && before.boot != boot;
        if secs < self.config.idle_close_secs && !restart {
            return String::new();
        }
        if secs < self.config.idle_close_secs {
            return "(after a restart)".to_string();
        }
        gap_note(secs, restart)
    }

    /// py: telegram.py:4922 `def _log_history(self) -> list[dict[str, Any]]:`
    pub(crate) fn log_history(self: &Arc<Self>) -> Vec<Obj> {
        let app = ctx().app.is_some(); // an app turn reads eve's blocks as written (history_text)
        let (window, older) = self.log_window();
        let mut items = if older { vec![message_item("developer", OLDER_NOTE)] } else { Vec::new() };
        let mut prev: Option<&LogLine> = None;
        for ln in &window {
            if let Some(p) = prev {
                let gap = self.gap(p, ln.t, &ln.boot);
                if !gap.is_empty() {
                    items.push(message_item("developer", &gap));
                }
            }
            if ln.who == "event" && ln.kind == AGENT_KIND {
                let label = ln.text.split_once(": ").map(|(l, _)| l).unwrap_or(&ln.text);
                items.push(message_item("developer", &AGENT_NOTE.replace("{label}", label)));
            } else if ln.who == "event" {
                items.push(message_item("developer", &format!("({})", ln.text)));
            } else if ln.who == "user" {
                items.push(message_item("user", &ln.text));
            } else {
                items.push(message_item("assistant", &history_text(&ln.text, app)));
            }
            prev = Some(ln);
        }
        items
    }

    /// py: telegram.py:4943 `def _gap_before_now(self, now: float) -> list[dict[str, Any]]:`
    ///
    /// The gap note between the history's newest line and a message arriving at ``now``: [] or one item.
    /// The same words the next turn's history computes from the stored times, so the cache holds.
    pub(crate) fn gap_before_now(self: &Arc<Self>, now: f64) -> Vec<Obj> {
        let Some(log) = self.chat_log_now() else {
            return Vec::new();
        };
        let (window, _) = self.log_window();
        let Some(last) = window.last() else {
            return Vec::new();
        };
        let gap = self.gap(last, now, &log.boot);
        if gap.is_empty() {
            Vec::new()
        } else {
            vec![message_item("developer", &gap)]
        }
    }

    /// py: telegram.py:4954 `def _shown_lines(self) -> list[Any]:`
    ///
    /// The log lines this turn's input shows: the window its history rendered (_log_window, already run this
    /// turn) and every line written since, the message being answered among them. Never moves the window: a
    /// second cut here, after the new message, would leave lines the history showed out of the count, and the
    /// today block would carry them again (review, 2026-10-08).
    pub(crate) fn shown_lines(self: &Arc<Self>) -> Vec<LogLine> {
        let Some(log) = self.chat_log_now() else {
            return Vec::new();
        };
        let (gen, seq) = {
            let st = self.st.lock();
            (st.window_gen, st.window_seq)
        };
        if gen != log.generation() as i64 {
            return self.log_window().0; // a forget since the history: read it afresh
        }
        log.lines(self.log_chat()).into_iter().filter(|ln| ln.seq >= seq).collect()
    }

    /// py: telegram.py:4964 `def _shown_convs(self) -> dict[str, int]:`
    ///
    /// With the chat log: per transcript conversation, how many of its say/image lines the history shows,
    /// the ones today's block must leave out (a line is in the prompt once, never twice).
    pub(crate) fn shown_convs(self: &Arc<Self>) -> indexmap::IndexMap<String, i64> {
        let mut out = indexmap::IndexMap::new();
        if self.chat_log_now().is_none() {
            return out;
        }
        for ln in self.shown_lines() {
            if !ln.conv.is_empty() && (ln.kind == "say" || ln.kind == "image") {
                *out.entry(ln.conv.clone()).or_insert(0) += 1;
            }
        }
        out
    }

    /// py: telegram.py:4975 `def _record(self, who: str, kind: str, text: str, tool: str = "", att: Optional[list[dict[str, str]]] = None, outside: bool = False, src: Optional[list[dict[str, str]]] = None) -> bool:`
    ///
    /// One line into the transcript only (a relay line, a tool result) → whether it was written. Nothing
    /// without a Memory lent. The chat's conversation id is minted by the first line after a close. A ``media``
    /// line (a picture or file eve sent, _keep_for_app) is not talk: it keeps an open chat's quiet clock as it
    /// was, so a task's or the chief's picture never puts off the idle close; it starts the clock only when it
    /// opened the conversation itself, so that one still closes.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record(self: &Arc<Self>, who: &str, kind: &str, text: &str, tool: &str, att: Option<&[HashMap<String, String>]>, outside: bool, src: Option<&[HashMap<String, String>]>) -> bool {
        let Some(memory) = self.memory.clone() else {
            return false;
        };
        let store = memory.transcripts();
        let (conv, opened) = {
            let mut st = self.st.lock();
            match st.conv.clone() {
                Some(conv) => (conv, false),
                None => {
                    let conv = store.new_conv(CHANNEL);
                    st.conv = Some(conv.clone());
                    (conv, true)
                }
            }
        };
        // A line of an app turn says so (transcripts ``via``), so Eve Link reads it back as the app's.
        let app = ctx().app;
        let mut opts = eve_transcripts::Append { tool: tool.to_string(), ..Default::default() };
        if let Some(turn) = &app {
            opts.via = "app".into();
            if who == "user" {
                opts.mid = turn.clone(); // where its reaction is kept (link_reactions.py)
            }
        }
        if let Some(att) = att.filter(|a| !a.is_empty()) {
            opts.att = Some(str_maps(att)); // the app's attachments (link_chat._att reads them)
        }
        if outside {
            opts.outside = true; // kept as outside words (transcripts ``out``)
        }
        if let Some(src) = src.filter(|s| !s.is_empty()) {
            opts.src = Some(Value::Array(str_maps(src))); // the reply's memory sources (memory_sources.py)
        }
        let who_on_disk = if who == "user" { "owner" } else { who };
        let written = store.append_with(CHANNEL, &conv, who_on_disk, kind, text, opts);
        let mut st = self.st.lock();
        if kind != "media" || opened || st.last_turn_at.is_none() {
            st.last_turn_at = Some((self.clock)());
        }
        written
    }

    /// py: telegram.py:5009 `def _close_quiet_chat(self) -> None:`
    pub fn close_quiet_chat(self: &Arc<Self>) {
        let (open, last) = {
            let st = self.st.lock();
            (!st.turns.is_empty() || st.conv.is_some(), st.last_turn_at)
        };
        if let Some(last) = last {
            if open && !self.task_running() && (self.clock)() - last >= self.config.idle_close_secs {
                self.close_chat();
            }
        }
    }

    /// py: telegram.py:5014 `def _close_chat(self) -> None:`
    ///
    /// The chat is over (ten quiet minutes, or the daemon stopping): the transcript gets its close marker
    /// and the RAM history is cleared. Nothing is handed anywhere: the words are already on disk, and the
    /// nightly dream reads them from there.
    pub(crate) fn close_chat(self: &Arc<Self>) {
        let conv = {
            let mut st = self.st.lock();
            if st.chat_log.is_none() {
                st.turns.clear();
            }
            // with the chat log the history stays: the next message shows the quiet spell as a gap note instead
            st.turn_kinds.clear();
            st.last_turn_at = None;
            st.conv.take()
        };
        if let (Some(conv), Some(memory)) = (conv, self.memory.as_ref()) {
            // a marker the store declines (off, a bad conversation id) is its own business, as in Python
            let _ = memory.transcripts().close(CHANNEL, &conv, None);
        }
    }

    /// py: telegram.py:5029 `def _history(self) -> list[dict[str, Any]]:`
    pub fn history(self: &Arc<Self>) -> Vec<Obj> {
        if self.chat_log_now().is_some() {
            return self.log_history();
        }
        let app = ctx().app.is_some();
        let turns = self.st.lock().turns.clone();
        let from = turns.len().saturating_sub(HISTORY_TURNS);
        turns[from..]
            .iter()
            .map(|(who, text)| {
                if who == "user" {
                    message_item("user", text)
                } else {
                    message_item("assistant", &history_text(text, app))
                }
            })
            .collect()
    }

    /// py: telegram.py:5037 `def _shown(kinds: Sequence[str]) -> int:`
    ///
    /// How many of `kinds` are say/image transcript lines: the ones the today block must leave out
    /// because the history already shows them, so no line is in the prompt twice and none zero times.
    pub fn shown(kinds: &[String]) -> i64 {
        kinds.iter().filter(|k| k.as_str() == "say" || k.as_str() == "image").count() as i64
    }

    /// py: telegram.py:5042 `def _memory_tools(self) -> list[dict[str, Any]]:`
    ///
    /// The memory's tool schemas, read once (they are fixed, and part of the cached prefix).
    pub(crate) fn memory_tools(self: &Arc<Self>) -> Vec<Obj> {
        let Some(memory) = self.memory.as_ref() else {
            return Vec::new();
        };
        let cached = self.st.lock().memory_tools_cache.clone();
        let tools = match cached {
            Some(tools) => tools,
            None => {
                let tools = memory.tools(false);
                self.st.lock().memory_tools_cache = Some(tools.clone());
                tools
            }
        };
        tools.into_iter().filter_map(|t| t.as_object().cloned()).collect()
    }

    /// py: telegram.py:5054 `def _prompt_memory(self, tail: int, *, history: bool = True) -> tuple[str, str]:`
    ///
    /// (profile, today) for one turn, read off the loop. The profile is a file the owner may edit between
    /// two texts, and today grows with every line on either channel: both are read per turn, never cached.
    /// Today leaves out this chat's newest `tail` lines, the ones its history already shows. ``history`` False:
    /// the request shows no history (a rundown), so only the message being answered is left out.
    pub(crate) fn prompt_memory(self: &Arc<Self>, tail: i64, history: bool) -> (String, String) {
        let Some(memory) = self.memory.clone() else {
            return (String::new(), String::new());
        };
        let profile = memory.profile();
        let max = TODAY_CHARS as i64;
        // With nothing shown there is nothing to leave out: exclude_tail=0 would hide the whole chat,
        // lines the history no longer has among them. With the chat log the count is what the request
        // shows (_shown_convs), whatever the turn kinds say: a call's warm-up after a quiet spell has none
        // left (_close_chat), yet its history shows the log's window (review, 2026-10-08).
        if self.chat_log_now().is_none() || !history {
            // no log, or a rundown (its input is the message alone): only the newest `tail` lines are shown
            let conv = if tail > 0 { self.st.lock().conv.clone() } else { None };
            let options = eve_transcripts::Today { exclude_conv: conv, exclude_tail: tail, ..Default::default() };
            return (profile, memory.today(max, &options));
        }
        let shown = self.shown_convs();
        let today = if shown.len() <= 1 {
            let (conv, n) = shown.iter().next().map(|(c, n)| (Some(c.clone()), *n)).unwrap_or((None, 0));
            memory.today(max, &eve_transcripts::Today { exclude_conv: conv, exclude_tail: n, ..Default::default() })
        } else {
            // the history spans conversations (a quiet spell closed one): each one's shown lines are left out
            let exclude: Vec<(String, i64)> = shown.into_iter().collect();
            memory.today(max, &eve_transcripts::Today { exclude, ..Default::default() })
        };
        (profile, today)
    }

    /// py: telegram.py:5088 `def _think_context(self) -> str:`
    ///
    /// What think_hard is told about the owner: the voice-sized profile (the record index is no use to
    /// it), then as much of today as fits beside it in think.py's cap. The profile is the owner; today is
    /// what the question is most likely about, so today is never the part cut off.
    pub(crate) fn think_context(self: &Arc<Self>) -> String {
        let Some(memory) = self.memory.as_ref() else {
            return String::new();
        };
        let profile = memory.profile_for_voice().trim().to_string();
        let used = if profile.is_empty() { 0 } else { profile.chars().count() as i64 + 2 };
        let room = THINK_CONTEXT_CHARS as i64 - used;
        let today = if room > 200 {
            memory.today(room, &eve_transcripts::Today::default()).trim().to_string()
        } else {
            String::new()
        };
        [profile, today].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join("\n\n")
    }
}
