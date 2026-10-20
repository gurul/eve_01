//! The chief's proof: when a card is done, decided by code from evidence the executor did not write.
//! Port of `chief_receipt.py`.
//!
//! Every executor hands back one shape, [`PhaseResult`] (defined in eve-chief-card beside the Phase that carries
//! it, re-exported here): a closed status, a list of evidence, and `text`, the executor's own words. The oracles
//! below read only the evidence, and each says one word from plan_executor's closed set ([`OUTCOMES`]):
//!
//! * `confirmed`     the evidence shows it.
//! * `unverifiable`  nothing could be read back, or what was read does not show it. Never counted as success.
//! * `failed`        the evidence shows it did not happen, or the phase itself failed, was refused or ran out of
//!   budget.
//!
//! A card is `done` only when every one of its checks is confirmed AND every one of its phases ran ("ok"); one
//! unverifiable or failed check, or one step that failed, was refused or was skipped, makes it `unverified`, never
//! done (design principle 4, "done means evidence"). The checks alone were not enough: a desk job checked only by
//! read_links and cited_pick closed done while its order had failed (reviewer, 2026-09-29). `text` never decides
//! anything: "I ordered it, confirmation #123" with no evidence is unverifiable.
//!
//! The checks (`DONE_KINDS_LIVE`) and what each reads:
//!
//! * `read_links(n)`  the distinct http(s) links the card's research phases read: the search's sources, the set
//!   search_router.keep_read_links trusts. n or more confirms.
//! * `cited_pick`     the links the assess phase's pick cites, every one of them in that read set. A pick that
//!   cites a link nobody read fails (the watcher's link lesson, WatchLink.lean).
//! * `ui_seen(text)`  the UI-state text a Mac executor captured, never its closing sentence: Codex's
//!   `ui_evidence` (an empty list means "not verified"). Only the LAST state of each act is read, the screen the
//!   act ended on: a page seen before the act ("Button: Order now" on the product page) proves nothing about it.
//!   The text is at least two words (`MIN_UI_WORDS`). Limit: Codex's evidence is any js tool output holding
//!   "Window:", which the agent's own script could print; that is codex_computer's to close, not this oracle's.
//! * `watch_armed`    the watcher's own row id, w<N> (returned by Watcher.add); when the caller lends the live
//!   watch ids, the row must be in them.
//! * `file_exists`    a regular file below the home folder (never the home folder itself), looked at by this code,
//!   made or changed at or after the card was: a file that was already there proves nothing.
//! * `guru_says_done` the owner's own Done tap or "done c<N>", matched by code. The receipt labels it "your call".
//!
//! The receipt is composed by code from the verdicts, so it cannot claim more than they do.

mod py;

use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use eve_chief_card::{
    home_path, is_watch_id, Card, Check, ValueError, MAX_EVIDENCE, MAX_EVIDENCE_REF_CHARS, MAX_RESULT_TEXT_CHARS,
    MIN_UI_WORDS,
};
pub use eve_chief_card::{Evidence, PhaseResult};

pub const OUTCOMES: &[&str] = &["confirmed", "unverifiable", "failed"];
/// The phase itself did not happen.
pub const FAILED_RESULTS: &[&str] = &["refused", "failed", "over_budget"];

/// `^https?://[^\s/]+\.[^\s]+$`, re.I. `\s` is Python's str whitespace (with FS, GS, RS, US), and `\n?` keeps
/// Python's `$`, which also matches before a final newline.
static URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^https?://[^\s\x1c-\x1f/]+\.[^\s\x1c-\x1f]+\n?$").expect("_URL")
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub check: Check,
    /// OUTCOMES
    pub outcome: String,
    /// one line for the receipt: what was read
    pub why: String,
}

impl Verdict {
    pub fn new(check: &Check, outcome: &str, why: impl Into<String>) -> Self {
        Verdict { check: check.clone(), outcome: outcome.to_string(), why: why.into() }
    }
}

/// `check`'s and `verify`'s keyword arguments, with Python's defaults. `owner_done`: the owner tapped Done (code
/// matched it). `live_watches`: the watcher's current ids, when the caller has them. `home`: the home folder
/// (tests); None is the account's.
#[derive(Debug, Clone, Copy, Default)]
pub struct CheckOptions<'a> {
    pub owner_done: bool,
    pub live_watches: Option<&'a HashSet<String>>,
    pub home: Option<&'a Path>,
}

fn link(r: &str) -> String {
    let text = py::strip(r).trim_end_matches(['.', ',', ';', ':', '!', '?']);
    if URL.is_match(text) {
        text.to_string()
    } else {
        String::new()
    }
}

fn results<'a>(card: &'a Card, kind: &str) -> Vec<&'a PhaseResult> {
    card.phases.iter().filter(|p| p.r#do == kind).filter_map(|p| p.result.as_ref()).collect()
}

/// The links the card's research phases read (done results only).
pub fn read_set(card: &Card) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for r in results(card, "research") {
        if r.status == "done" {
            out.extend(r.evidence.iter().filter(|e| e.kind == "links").map(|e| link(&e.r#ref)).filter(|x| !x.is_empty()));
        }
    }
    out
}

fn gate(check: &Check, results: &[&PhaseResult], kind: &str) -> Option<Verdict> {
    if results.is_empty() {
        return Some(Verdict::new(check, "unverifiable", format!("no {kind} result yet")));
    }
    if results.iter().any(|r| r.status == "done") {
        return None;
    }
    if let Some(worst) = results.iter().find(|r| FAILED_RESULTS.contains(&r.status.as_str())) {
        return Some(Verdict::new(check, "failed", format!("the {kind} phase {}", worst.status.replace('_', " "))));
    }
    Some(Verdict::new(check, "unverifiable", format!("the {kind} phase was handed on")))
}

/// `st_mtime`: seconds and nanoseconds as CPython adds them.
fn mtime(meta: &fs::Metadata) -> f64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.mtime() as f64 + meta.mtime_nsec() as f64 * 1e-9
    }
    #[cfg(not(unix))]
    {
        use std::time::UNIX_EPOCH;
        match meta.modified() {
            Ok(t) => match t.duration_since(UNIX_EPOCH) {
                Ok(d) => d.as_secs_f64(),
                Err(e) => -e.duration().as_secs_f64(),
            },
            Err(_) => 0.0,
        }
    }
}

/// One check against the card's evidence. Err is the ValueError Python lets out: a file_exists path with a NUL
/// in it, or a hand-edited read_links count `int()` cannot read.
pub fn check(check: &Check, card: &Card, opts: &CheckOptions<'_>) -> Result<Verdict, ValueError> {
    let v = |outcome: &str, why: String| Ok(Verdict::new(check, outcome, why));
    match check.kind.as_str() {
        "guru_says_done" => {
            if opts.owner_done {
                v("confirmed", "your call".into())
            } else {
                v("unverifiable", "waiting for your word".into())
            }
        }
        "file_exists" => {
            let Some(path) = home_path(&check.arg, opts.home)? else {
                return v("failed", "not a file in the home folder".into());
            };
            let Ok(meta) = fs::metadata(&path) else {
                return v("failed", "the file is not there".into());
            };
            if !meta.is_file() {
                return v("failed", "that is not a file".into());
            }
            if mtime(&meta) < card.created {
                return v("unverifiable", "the file is from before the job".into());
            }
            v("confirmed", "the file is there".into())
        }
        "read_links" => {
            let rs = results(card, "research");
            if let Some(g) = gate(check, &rs, "research") {
                return Ok(g);
            }
            let raw = if check.arg.is_empty() { "1" } else { check.arg.as_str() };
            let n = py::int(raw).map_err(ValueError)?;
            let got = read_set(card).len();
            let why = format!("read {got} page{}", if got != 1 { "s" } else { "" });
            v(if got as i128 >= n { "confirmed" } else { "failed" }, why)
        }
        "cited_pick" => {
            let rs = results(card, "assess");
            if let Some(g) = gate(check, &rs, "assess") {
                return Ok(g);
            }
            let cited: BTreeSet<String> = rs
                .iter()
                .filter(|r| r.status == "done")
                .flat_map(|r| r.evidence.iter().filter(|e| e.kind == "links").map(|e| link(&e.r#ref)))
                .filter(|x| !x.is_empty())
                .collect();
            if cited.is_empty() {
                return v("failed", "the pick cites no page".into());
            }
            let read = read_set(card);
            let unread = cited.iter().filter(|x| !read.contains(*x)).count();
            if unread > 0 {
                return v("failed", format!("the pick cites {unread} page(s) I did not read"));
            }
            v("confirmed", "the pick cites a page I read".into())
        }
        "ui_seen" => {
            let rs = results(card, "act");
            if let Some(g) = gate(check, &rs, "act") {
                return Ok(g);
            }
            let want = py::squash(&py::casefold(&check.arg));
            if py::split(&want).count() < MIN_UI_WORDS {
                return v("unverifiable", format!("'{}' is too short to prove anything", check.arg));
            }
            let mut states = Vec::new();
            for r in rs.iter().filter(|r| r.status == "done") {
                // the screen this act ended on
                if let Some(e) =
                    r.evidence.iter().rev().find(|e| e.kind == "ui_text" && !py::strip(&e.r#ref).is_empty())
                {
                    states.push(&e.r#ref);
                }
            }
            if states.is_empty() {
                return v("unverifiable", "no screen state was read back".into());
            }
            if states.iter().any(|s| py::squash(&py::casefold(s)).contains(&want)) {
                return v("confirmed", format!("'{}' seen on screen", check.arg));
            }
            v("unverifiable", format!("'{}' not seen on screen", check.arg))
        }
        "watch_armed" => {
            let rs = results(card, "watch");
            if let Some(g) = gate(check, &rs, "watch") {
                return Ok(g);
            }
            let rows: Vec<&str> = rs
                .iter()
                .filter(|r| r.status == "done")
                .flat_map(|r| r.evidence.iter().filter(|e| e.kind == "watch_row").map(|e| py::strip(&e.r#ref)))
                .filter(|x| is_watch_id(x))
                .collect();
            let Some(first) = rows.first() else {
                return v("failed", "no watch was set".into());
            };
            if let Some(live) = opts.live_watches {
                if !rows.iter().any(|x| live.contains(*x)) {
                    return v("failed", format!("watch {first} is not on the list"));
                }
            }
            v("confirmed", format!("watch {first} is set"))
        }
        _ => v("unverifiable", "no oracle for this check".into()),
    }
}

/// Every done check of the card, in order.
pub fn verify(card: &Card, opts: &CheckOptions<'_>) -> Result<Vec<Verdict>, ValueError> {
    card.done_checks.iter().map(|c| check(c, card, opts)).collect()
}

/// "done" only when there is at least one verdict and every one is confirmed; else "unverified". The checks
/// only: a card's status is [`card_status`].
pub fn status_of(verdicts: &[Verdict]) -> &'static str {
    if !verdicts.is_empty() && verdicts.iter().all(|v| v.outcome == "confirmed") {
        "done"
    } else {
        "unverified"
    }
}

/// (n, kind, status) of each phase that did not run to "ok": failed, refused, skipped, or never finished.
pub fn steps_not_run(card: &Card) -> Vec<(usize, String, String)> {
    card.phases.iter().filter(|p| p.status != "ok").map(|p| (p.n, p.r#do.clone(), p.status.clone())).collect()
}

/// "done" only when every check is confirmed and every phase ran ("ok"); else "unverified".
pub fn card_status(card: &Card, verdicts: &[Verdict]) -> &'static str {
    if status_of(verdicts) == "done" && steps_not_run(card).is_empty() {
        "done"
    } else {
        "unverified"
    }
}

/// The card's status once its phases have run: "done" or "unverified".
pub fn close(card: &Card, opts: &CheckOptions<'_>) -> Result<&'static str, ValueError> {
    Ok(card_status(card, &verify(card, opts)?))
}

/// `f"{max(1, round(secs / 60))} min" if secs >= 30 else f"{secs:.0f} s"`; `round` is half to even.
fn minutes(secs: f64) -> String {
    if secs >= 30.0 {
        let m = (secs / 60.0).round_ties_even().max(1.0);
        format!("{} min", py::fixed(m, 0))
    } else {
        format!("{} s", py::fixed(secs, 0))
    }
}

/// "Done: standing desk, 3 of 3 checks: read 4 pages (confirmed); … $0.31, 12 min." Composed from the
/// verdicts; an unverified card names what could not be confirmed and asks.
pub fn receipt(card: &Card, verdicts: &[Verdict]) -> String {
    let ok = verdicts.iter().filter(|v| v.outcome == "confirmed").count();
    let status = card_status(card, verdicts);
    let head = if status == "done" { "Done" } else { "Not confirmed" };
    let parts = verdicts.iter().map(|v| format!("{} ({})", v.why, v.outcome)).collect::<Vec<_>>().join("; ");
    let steps: String =
        steps_not_run(card).iter().map(|(n, d, st)| format!(" Step {n} ({d}) {st}.")).collect();
    // Python's truth of a float: NaN is true, as `!= 0.0` is.
    let cost = if card.spent.usd != 0.0 || card.spent.wall_s != 0.0 {
        format!(" ${}, {}.", py::fixed(card.spent.usd, 2), minutes(card.spent.wall_s))
    } else {
        String::new()
    };
    let mut text = format!("{head}: {}, {ok} of {} checks: {parts}.{steps}{cost}", card.title, verdicts.len());
    if status != "done" {
        text.push_str(" Did it go through?");
    }
    text
}

// ---- adapters: an executor's output in, the one shape out ---------------------------------------

/// `str(text or "")[:limit]`.
fn cap(text: Option<&Value>, limit: usize) -> String {
    py::take_chars(&py::str_or_empty(text), limit).to_string()
}

/// A positive int or float (never a bool) as a float; else 0.0.
fn usd(value: Option<&Value>) -> f64 {
    match value {
        Some(Value::Number(n)) => n.as_f64().filter(|f| *f > 0.0).unwrap_or(0.0),
        _ => 0.0,
    }
}

/// `max(0.0, secs)`: the first argument unless the second is greater (so NaN and -0.0 give 0.0).
fn non_negative(secs: f64) -> f64 {
    if secs > 0.0 {
        secs
    } else {
        0.0
    }
}

fn result(status: &str, evidence: Vec<Evidence>, text: String, usd: f64, secs: f64) -> PhaseResult {
    PhaseResult { status: status.to_string(), evidence, text, usd, secs }
}

/// search_router.answer's result (`{"ok", "answer", "sources": [{"url"}], "cost_usd"}`) as a research result.
/// The sources are the links read; the answer is text.
pub fn from_search(out: &Value, secs: f64) -> PhaseResult {
    let Some(map) = out.as_object().filter(|m| py::truthy(m.get("ok"))) else {
        let reason = out.as_object().and_then(|m| m.get("reason"));
        return result("failed", Vec::new(), cap(reason, MAX_RESULT_TEXT_CHARS), 0.0, non_negative(secs));
    };
    let mut links: Vec<String> = Vec::new();
    // `out.get("sources") or []`: a list's items; a string's characters and an object's keys are not mappings,
    // so they give no link.
    if let Some(Value::Array(sources)) = map.get("sources") {
        for s in sources {
            let url = match s.as_object() {
                Some(m) => link(&py::str_or_empty(m.get("url"))),
                None => String::new(),
            };
            if !url.is_empty() && !links.contains(&url) {
                links.push(url);
            }
        }
    }
    let evidence = links
        .iter()
        .take(MAX_EVIDENCE)
        .map(|u| Evidence::new("links", py::take_chars(u, MAX_EVIDENCE_REF_CHARS)))
        .collect();
    result("done", evidence, cap(map.get("answer"), MAX_RESULT_TEXT_CHARS), usd(map.get("cost_usd")), non_negative(secs))
}

/// `from_agent`'s keyword arguments, with Python's defaults (`ok` true).
#[derive(Debug, Clone, Copy)]
pub struct AgentRun {
    pub ok: bool,
    pub usd: f64,
    pub secs: f64,
    pub act: bool,
}

impl Default for AgentRun {
    fn default() -> Self {
        AgentRun { ok: true, usd: 0.0, secs: 0.0, act: false }
    }
}

/// A Mac executor's run: its closing sentence as text, its captured UI states as evidence.
/// `ok` false (the run failed, was stopped, or its agent reported an error) makes the result failed whatever it
/// said. `act`: an act that ran with no UI state seen is `handed_on`, never done: Codex returns its failure
/// sentence instead of raising, and the sentence alone is no evidence (reviewer after P4, 2026-09-29: such a run
/// closed a card done).
pub fn from_agent(final_text: &str, ui_evidence: &[Value], run: &AgentRun) -> PhaseResult {
    let mut states = Vec::new();
    for item in ui_evidence {
        let state = match item.as_object() {
            Some(m) => py::str_or_empty(m.get("state")),
            None => String::new(),
        };
        if !py::strip(&state).is_empty() {
            states.push(Evidence::new("ui_text", py::take_chars(&state, MAX_EVIDENCE_REF_CHARS)));
        }
    }
    let status = if !run.ok {
        "failed"
    } else if run.act && states.is_empty() {
        "handed_on"
    } else {
        "done"
    };
    let usd = if run.usd > 0.0 { run.usd } else { 0.0 };
    result(
        status,
        py::last(states, MAX_EVIDENCE),
        py::take_chars(final_text, MAX_RESULT_TEXT_CHARS).to_string(),
        usd,
        non_negative(run.secs),
    )
}

/// Watcher.add's result (`{"ok", "id": "w<N>", …}`) as a watch result.
pub fn from_watch(out: &Value) -> PhaseResult {
    if let Some(map) = out.as_object() {
        let id = py::str_or_empty(map.get("id"));
        if py::truthy(map.get("ok")) && is_watch_id(&id) {
            return result("done", vec![Evidence::new("watch_row", &id)], cap(map.get("watching_for"), 200), 0.0, 0.0);
        }
    }
    let reason = out.as_object().and_then(|m| m.get("reason"));
    result("failed", Vec::new(), cap(reason, MAX_RESULT_TEXT_CHARS), 0.0, 0.0)
}
