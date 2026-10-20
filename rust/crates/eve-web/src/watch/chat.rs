//! watch.py's `Watcher` chat tools: watch_add / list / remove / pause / resume / set_end, and /watches.

use std::panic::AssertUnwindSafe;
use std::sync::{Arc, LazyLock};

use futures::FutureExt as _;
use regex::Regex;
use serde_json::{json, Map, Value};
use tracing::error;

use super::net::check_url;
use super::rules::{describe, every_words, hours, now_line, stretch, when, Watch};
use super::tools::{INSTRUCTIONS_BLOCK, TICKETMASTER_LINE, TOOLS};
use super::watcher::{panic_text, py_split, py_str, py_strip, WatchRef, Watcher};
use super::{
    default_every_secs, floor_secs, CONDITIONS, FIRST_CHECK_WAIT_SECS, KINDS, MAX_EVERY_SECS, MAX_WATCHES,
    PRICE_CONDITIONS, TELL_AFTER_ERRORS,
};

static TM_TARGET_WORDS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r" ticketmaster: [^.]*(?:\.com [^.]*)?\.").expect("ticketmaster target words"));
static QUOTE_SYMBOL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9.^=\-]{1,20}$").expect("symbol"));
static WATCH_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^w[0-9]{1,9}$").expect("watch id"));

/// `" ".join(str(x or "").split())[:n]`.
fn collapsed(v: Option<&Value>, n: usize) -> String {
    py_split(&py_str(v)).join(" ").chars().take(n).collect()
}

/// A JSON number that is not a bool, as a float (`isinstance(x, (int, float)) and not isinstance(x, bool)`).
fn number(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Number(n)) => n.as_f64(),
        _ => None,
    }
}

impl Watcher {
    /// The kinds this computer can watch: ticketmaster needs its key.
    pub fn kinds(&self) -> Vec<&'static str> {
        KINDS.iter().copied().filter(|k| *k != "ticketmaster" || self.config.ticketmaster).collect()
    }

    /// INSTRUCTIONS_BLOCK, with TICKETMASTER_LINE before "The condition:" when ticketmaster is on offer.
    pub fn instructions(&self) -> String {
        let extra = if self.kinds().contains(&"ticketmaster") { TICKETMASTER_LINE } else { "" };
        INSTRUCTIONS_BLOCK.replacen("\nThe condition:", &format!("{extra}\nThe condition:"), 1)
    }

    /// The schemas, naming only the kinds on offer, so the model never picks one this computer lacks.
    pub fn tools(&self) -> Value {
        let mut out = TOOLS.clone();
        let kinds = self.kinds();
        if let Some(props) = out
            .get_mut(0)
            .and_then(|t| t.get_mut("parameters"))
            .and_then(|p| p.get_mut("properties"))
            .and_then(Value::as_object_mut)
        {
            if let Some(kind) = props.get_mut("kind").and_then(Value::as_object_mut) {
                kind.insert("enum".into(), json!(kinds));
            }
            if let Some(target) = props.get_mut("target").and_then(Value::as_object_mut) {
                let mut words = target.get("description").and_then(Value::as_str).unwrap_or("").to_string();
                if !kinds.contains(&"ticketmaster") {
                    words = TM_TARGET_WORDS.replace_all(&words, "").into_owned();
                }
                target.insert("description".into(), json!(words));
            }
        }
        out
    }

    /// One tool call by name: its JSON answer.
    pub async fn handle(&self, name: &str, args: &Map<String, Value>) -> Value {
        let id = || py_str(args.get("id"));
        let for_hours = args.get("for_hours").cloned().unwrap_or(Value::Null);
        match name {
            "watch_add" => self.add(args).await,
            "watch_list" => {
                let list: Vec<Value> = self.watches().iter().map(|w| self.summary(&w.lock())).collect();
                json!({"ok": true, "watches": list})
            }
            "watch_remove" => self.remove(&id()),
            "watch_pause" => self.pause(&id(), &for_hours),
            "watch_resume" => self.resume(&id()),
            "watch_set_end" => self.set_end(&id(), &for_hours),
            _ => json!({"ok": false, "reason": format!("unknown watch tool {name}")}),
        }
    }

    /// `_find(wid)`: by id, case and spacing aside.
    pub(crate) fn find(&self, wid: &str) -> Option<WatchRef> {
        let wid = py_strip(wid).to_lowercase();
        self.watches().into_iter().find(|w| w.lock().id == wid)
    }

    /// `_no_such(wid)`.
    pub(crate) fn no_such(&self, wid: &str) -> Value {
        json!({"ok": false, "reason": format!("no watch with id {}; watch_list shows the ids", py_repr(&py_strip(wid).to_lowercase()))})
    }

    /// No checks and no alerts until resumed, or until `for_hours` run out; the watch keeps its state.
    pub fn pause(&self, wid: &str, for_hours: &Value) -> Value {
        let Some(w) = self.find(wid) else {
            return self.no_such(wid);
        };
        let (h, why) = hours(for_hours);
        if !why.is_empty() {
            return json!({"ok": false, "reason": why});
        }
        let (label, until) = {
            let mut g = w.lock();
            g.paused = true;
            g.paused_until = match h {
                Some(h) if h != 0.0 => self.now() + h * 3600.0,
                _ => 0.0,
            };
            (g.label.clone(), g.paused_until)
        };
        self.save();
        self.wake_up();
        let until = if until != 0.0 { when(until) } else { "until you resume it".to_string() };
        json!({"ok": true, "paused": label, "until": until})
    }

    pub fn resume(&self, wid: &str) -> Value {
        let Some(w) = self.find(wid) else {
            return self.no_such(wid);
        };
        let label = {
            let mut g = w.lock();
            if !g.paused {
                return json!({"ok": true, "resumed": g.label, "note": "it was not paused"});
            }
            g.paused = false;
            g.paused_until = 0.0;
            g.next_at = self.now();
            g.label.clone()
        };
        self.save();
        self.wake_up();
        json!({"ok": true, "resumed": label, "note": "checked again right away"})
    }

    pub fn set_end(&self, wid: &str, for_hours: &Value) -> Value {
        let Some(w) = self.find(wid) else {
            return self.no_such(wid);
        };
        let (h, why) = hours(for_hours);
        if !why.is_empty() {
            return json!({"ok": false, "reason": why});
        }
        let (label, ends) = {
            let mut g = w.lock();
            g.ends_at = match h {
                Some(h) if h != 0.0 => self.now() + h * 3600.0,
                _ => 0.0,
            };
            (g.label.clone(), g.ends_at)
        };
        self.save();
        self.wake_up();
        let ends = if ends != 0.0 { when(ends) } else { "no end".to_string() };
        json!({"ok": true, "label": label, "ends": ends})
    }

    /// `_summary(w)`: one watch for watch_list.
    pub(crate) fn summary(&self, w: &Watch) -> Value {
        let mut out = Map::new();
        out.insert("id".into(), json!(w.id));
        out.insert("label".into(), json!(w.label));
        out.insert("kind".into(), json!(w.kind));
        out.insert("watching_for".into(), json!(describe(w)));
        out.insert("now".into(), json!(now_line(w)));
        out.insert("every".into(), json!(every_words(w.every_secs)));
        out.insert("alerts_sent".into(), json!(w.fired));
        let via = match w.via.as_str() {
            "search" => Some("search (the site refuses automated reads)"),
            "browser" => Some("browser (rendered and looked at)"),
            "hosted" => Some("TinyFish (a hosted reader; the site refuses this Mac)"),
            _ => None,
        };
        if let Some(via) = via {
            out.insert("via".into(), json!(via));
        }
        if w.errors != 0 {
            out.insert("problem".into(), json!(w.last_error));
        }
        if w.paused {
            let until = if w.paused_until != 0.0 { when(w.paused_until) } else { "until resumed".to_string() };
            out.insert("paused".into(), json!(until));
        }
        if w.ends_at != 0.0 {
            out.insert("ends".into(), json!(when(w.ends_at)));
        }
        Value::Object(out)
    }

    pub fn remove(&self, wid: &str) -> Value {
        let wid = py_strip(wid).to_lowercase();
        let removed = {
            let mut st = self.state.lock();
            let at = st.watches.iter().position(|w| w.lock().id == wid);
            at.map(|i| st.watches.remove(i))
        };
        match removed {
            Some(w) => {
                self.save();
                json!({"ok": true, "removed": w.lock().label})
            }
            None => json!({"ok": false, "reason": format!("no watch with id {}; watch_list shows the ids", py_repr(&wid))}),
        }
    }

    /// `_new_id()`: one past the highest id ever handed out or on the list, never one in use.
    pub(crate) fn new_id(&self) -> String {
        let mut st = self.state.lock();
        let ids: Vec<String> = st.watches.iter().map(|w| w.lock().id.clone()).collect();
        let highest = ids
            .iter()
            .filter(|id| WATCH_ID.is_match(id))
            .filter_map(|id| id[1..].parse::<i64>().ok())
            .fold(st.last_id, i64::max);
        let mut n = highest + 1;
        while ids.iter().any(|id| *id == format!("w{n}")) {
            n += 1;
        }
        st.last_id = n;
        format!("w{n}")
    }

    /// A new watch from watch_add's arguments, or why not.
    pub fn validate(&self, args: &Map<String, Value>) -> Result<Watch, String> {
        let kind = py_strip(&py_str(args.get("kind"))).to_lowercase();
        let target = collapsed(args.get("target"), usize::MAX);
        let mut label = collapsed(args.get("label"), 80);
        if label.is_empty() {
            label = target.chars().take(80).collect();
        }
        let condition = py_strip(&py_str(args.get("condition"))).to_lowercase();
        let value = number(args.get("value"));
        let text = collapsed(args.get("text"), 200);
        let city = collapsed(args.get("city"), 60);
        if !KINDS.contains(&kind.as_str()) {
            return Err(format!("kind must be one of {}", self.kinds().join(", ")));
        }
        if kind == "ticketmaster" && !self.config.ticketmaster {
            return Err("Ticketmaster is not set up on this computer (TICKETMASTER_API_KEY); use kind search".into());
        }
        if !CONDITIONS.contains(&condition.as_str()) {
            return Err(format!("condition must be one of {}", CONDITIONS.join(", ")));
        }
        if target.is_empty() {
            return Err("nothing to watch: the target is empty".into());
        }
        if self.state.lock().watches.len() >= MAX_WATCHES {
            return Err(format!("already watching {MAX_WATCHES} things; remove one first"));
        }
        let priced = PRICE_CONDITIONS.contains(&condition.as_str());
        if priced && !value.is_some_and(|v| v.is_finite() && v > 0.0) {
            return Err(format!("{condition} needs a positive value"));
        }
        if condition == "drop_pct" && value.is_some_and(|v| v >= 100.0) {
            return Err("a drop of 100% or more cannot happen".into());
        }
        if condition == "appears" && text.is_empty() {
            return Err("appears needs the phrase to look for".into());
        }
        if condition == "appears" && kind != "page" {
            return Err("a phrase can only be watched on a page".into());
        }
        if kind == "quote" && (condition == "available" || condition == "appears") {
            return Err("a quote has a price only: use below, above, drop_pct, rise_pct or change".into());
        }
        if kind == "quote" && !QUOTE_SYMBOL.is_match(&target) {
            return Err("a quote target is a symbol like AAPL or BTC-USD".into());
        }
        if kind == "page" {
            let resolve = self.deps.resolve.clone();
            let why = check_url(&target, Some(&*resolve));
            if !why.is_empty() {
                return Err(why);
            }
        }
        if kind == "ticketmaster" && priced {
            return Err("Ticketmaster's API no longer publishes ticket prices reliably; watch availability here, or \
                        the event page (kind page) for its price"
                .into());
        }
        if kind == "ticketmaster" && target.chars().count() > 200 {
            return Err("a ticketmaster target is an artist, team or show, or an event link".into());
        }
        if (kind == "search" || kind == "ticketmaster") && condition == "appears" {
            return Err("a search cannot watch for a phrase".into());
        }
        let every = match number(args.get("every_minutes")) {
            Some(m) if m.is_finite() && m > 0.0 => (m * 60.0) as i64,
            _ => default_every_secs(&kind),
        };
        let every = floor_secs(&kind).max(MAX_EVERY_SECS.min(every));
        let (h, why) = hours(args.get("for_hours").unwrap_or(&Value::Null));
        if !why.is_empty() {
            return Err(why);
        }
        let mut w = Watch::new(&self.new_id(), &kind, &target, &label, &condition);
        w.value = value;
        w.text = text;
        w.city = if kind == "ticketmaster" { city } else { String::new() };
        w.every_secs = every;
        w.created = self.now();
        w.next_at = self.now();
        w.ends_at = match h {
            Some(h) if h != 0.0 => self.now() + h * 3600.0,
            _ => 0.0,
        };
        Ok(w)
    }

    /// Validate, check once (waiting up to FIRST_CHECK_WAIT_SECS for the limiter), then keep it. A first check that
    /// fails for a reason that will not go away (an unknown symbol, a bad link) keeps nothing.
    pub async fn add(&self, args: &Map<String, Value>) -> Value {
        let w = match self.validate(args) {
            Ok(w) => w,
            Err(why) => return json!({"ok": false, "reason": why}),
        };
        let w: WatchRef = Arc::new(parking_lot::Mutex::new(w));
        let host = self.host(&w.lock());
        let mut waited = 0.0;
        loop {
            let wait = self.limiter.lock().ready_in(&host);
            if !(wait > 0.0 && waited + wait <= FIRST_CHECK_WAIT_SECS) {
                break;
            }
            (self.deps.sleep)(wait).await;
            waited += wait;
        }
        let mut out = Map::new();
        {
            let g = w.lock();
            out.insert("ok".into(), json!(true));
            out.insert("id".into(), json!(g.id));
            out.insert("label".into(), json!(g.label));
            out.insert("watching_for".into(), json!(describe(&g)));
            out.insert("every".into(), json!(every_words(g.every_secs)));
        }
        let queued = "the first check is queued behind other requests to that site";
        if self.limiter.lock().ready_in(&host) > 0.0 {
            self.state.lock().watches.push(w.clone());
            self.save();
            self.wake_up();
            out.insert("now".into(), json!(queued));
            return Value::Object(out);
        }
        self.limiter.lock().take(&host);
        {
            let _one = self.lock.lock().await;
            if self.limiter.lock().backed_off(&host) > 0.0 {
                // a refusal landed on this host while the first check waited for the lock: keep it, queued
                out.insert("now".into(), json!(queued));
                self.state.lock().watches.push(w.clone());
                self.save();
                self.wake_up();
                return Value::Object(out);
            }
            let first = async {
                self.check(&w, true).await?;
                for _ in 0..3 {
                    let (checks, via, h) = {
                        let g = w.lock();
                        (g.checks, g.via.clone(), self.host(&g))
                    };
                    if checks != 0 || via.is_empty() || self.limiter.lock().ready_in(&h) > 0.0 {
                        break;
                    }
                    // the page refused a plain read: its first reading comes from the browser, TinyFish or the
                    // search, now (one re-check per rung moved down: tls -> browser -> hosted -> search)
                    self.limiter.lock().take(&h);
                    self.check(&w, true).await?;
                }
                Ok::<(), super::FetchError>(())
            };
            match AssertUnwindSafe(first).catch_unwind().await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    let (kind, target) = {
                        let g = w.lock();
                        (g.kind.clone(), g.target.clone())
                    };
                    if kind == "quote" && (e.status == 404 || e.reason.starts_with("no price")) {
                        return json!({"ok": false, "reason": format!("no such symbol on Yahoo Finance: {target}")});
                    }
                    if matches!(e.status, 400 | 404 | 410) && e.host.is_empty() {
                        return json!({"ok": false, "reason": e.reason}); // a link that is wrong now will stay wrong
                    }
                    out.insert("now".into(), json!(format!("the first check failed ({}); I'll keep trying", e.reason)));
                }
                Err(p) => {
                    // an answer shaped as no reader expected: kept, and retried later
                    let mut g = w.lock();
                    error!("watch: the first check of {} raised ({})", g.id, panic_text(p.as_ref()));
                    g.errors += 1;
                    g.last_error = "an answer I could not make sense of".to_string();
                    g.next_at = self.next(&g, stretch(g.errors));
                    out.insert(
                        "now".into(),
                        json!("the first check failed (an answer I could not make sense of); I'll keep trying"),
                    );
                }
            }
            self.state.lock().watches.push(w.clone());
            self.save();
        }
        self.wake_up();
        if !out.contains_key("now") {
            let g = w.lock();
            out.insert("now".into(), json!(now_line(&g)));
            let note = match g.via.as_str() {
                "search" => Some("that site refuses automated reads, so it is watched by web search instead"),
                "browser" => Some("that page only shows its price in a browser, so eve renders it and looks at it"),
                "hosted" => Some(
                    "that site refuses reads from this Mac, so eve reads it through TinyFish (a hosted reader, at most \
                     hourly)",
                ),
                _ => None,
            };
            if let Some(note) = note {
                out.insert("note".into(), json!(note));
            }
            if !g.last_note.is_empty() {
                out.insert("detail".into(), json!(g.last_note));
            }
            let met = matches!(g.condition.as_str(), "below" | "above" | "available" | "appears") && !g.armed;
            if met {
                out.insert(
                    "already".into(),
                    json!("the condition already holds now; the next alert comes after it stops and happens again"),
                );
            }
        }
        Value::Object(out)
    }

    /// /watches, by code: one line per watch.
    pub fn listing(&self) -> String {
        let watches = self.watches();
        if watches.is_empty() {
            return "I'm not watching anything. Text me something like \"tell me when AAPL drops below 300\".".to_string();
        }
        let mut lines = Vec::new();
        for w in &watches {
            let w = w.lock();
            let extra = match w.via.as_str() {
                "search" => " (by search)",
                "browser" => " (seen in a browser)",
                "hosted" => " (through TinyFish)",
                _ => "",
            };
            let problem =
                if w.errors >= TELL_AFTER_ERRORS { format!(" (can't read it: {})", w.last_error) } else { String::new() };
            let state = if w.paused && w.paused_until != 0.0 {
                format!(" · paused until {}", when(w.paused_until))
            } else if w.paused {
                " · paused".to_string()
            } else {
                String::new()
            };
            let end = if w.ends_at != 0.0 { format!(" · until {}", when(w.ends_at)) } else { String::new() };
            lines.push(format!(
                "{} · {}: {} · now {} · every {}{}{}{}{}",
                w.id,
                w.label,
                describe(&w),
                now_line(&w),
                every_words(w.every_secs),
                extra,
                problem,
                state,
                end
            ));
        }
        lines.push("Say \"stop watching w1\" to drop one, \"pause w1\" or \"resume w1\".".to_string());
        lines.join("\n")
    }
}

/// Python's `repr(str)`: single quotes unless the text holds one and no double quote; backslashes, the quote,
/// and non-printable characters escaped.
fn py_repr(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(q);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == q => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if (0x80..0xa0).contains(&(c as u32)) => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(q);
    out
}

#[cfg(test)]
mod tests {
    use super::py_repr;

    #[test]
    fn repr_quotes_as_python_does() {
        assert_eq!(py_repr("w9"), "'w9'");
        assert_eq!(py_repr("it's"), "\"it's\"");
        assert_eq!(py_repr("a\nb"), "'a\\nb'");
    }
}
