//! Caption pager: eve's text replies, one readable page at a time.
//!
//! The robot's screen has room for 4 lines of 17 characters under the eyes. A
//! reply streams in word by word from the model; this module turns the running
//! text into pages and decides *when* each page goes up and how long it stays,
//! at a reading pace rather than the model's writing pace:
//!
//! ```text
//!     hold(page)    = clamp(chars / read_cps + lead-if-first-page, 2 s, 9 s)
//!     deadline(i)   = max(t_shown + hold, t_frozen + 2 s)      -> next page shown here
//!     clear(last)   = max(t_shown + clamp(chars / read_cps + lead + 3 s, 6 s, 10 s), t_frozen + 3 s)
//! ```
//!
//! Page 0 of a reply fills word by word as the text arrives (refills throttled to
//! one per 0.15 s); a page is *frozen* once a later page has content or the reply
//! is final, and only a frozen page can turn. A reply that starts while a page is
//! still up queues behind it (the page on screen loses its last-page hold); a
//! barge-in drops everything. Every number lives in [`PagerConfig`].
//!
//! Pure: no async, no wall clock, no I/O. The caller passes a monotonic `now`
//! and sends the returned events to the board (`voice_agent`).
//!
//! Port of `bridge/src/eve_bridge/caption_pager.py`.

use serde_json::{json, Value};

pub const ELLIPSIS: &str = "…";

/// Every number the pager uses. Python: frozen dataclass `PagerConfig`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PagerConfig {
    /// size-3 Font0 cells: 7 + 17*18 = 313 px <= 314
    pub cols: usize,
    /// band y 112..204 at 23 px per line
    pub lines: usize,
    /// BBC 15-17 cps discounted for a 2" panel; env EVE_CAPTION_CPS (read by voice_agent)
    pub read_cps: f64,
    /// attention latency after the chirp (page 0 of a reply only)
    pub first_page_lead_secs: f64,
    /// a page flip must register as an event
    pub min_hold_secs: f64,
    pub max_hold_secs: f64,
    pub last_page_extra_secs: f64,
    pub last_page_min_secs: f64,
    pub last_page_max_secs: f64,
    /// a page's last line stays >= this after it stops changing
    pub freeze_dwell_secs: f64,
    /// page-0 refills throttled
    pub refill_interval_secs: f64,
    /// per reply; beyond it the text is dropped with an ellipsis
    pub max_pages: usize,
    /// what the model is asked to stay under (a 2-page budget, rounded)
    pub reply_budget_chars: usize,
}

impl Default for PagerConfig {
    fn default() -> Self {
        Self {
            cols: 17,
            lines: 4,
            read_cps: 12.0,
            first_page_lead_secs: 0.8,
            min_hold_secs: 2.0,
            max_hold_secs: 9.0,
            last_page_extra_secs: 3.0,
            last_page_min_secs: 6.0,
            last_page_max_secs: 10.0,
            freeze_dwell_secs: 2.0,
            refill_interval_secs: 0.15,
            max_pages: 6,
            reply_budget_chars: 100,
        }
    }
}

impl PagerConfig {
    /// The default config with a different reading pace (`PagerConfig(read_cps=...)`).
    pub fn with_read_cps(read_cps: f64) -> Self {
        Self { read_cps, ..Self::default() }
    }
}

/// One page: up to `lines` lines (Python: `tuple[str, ...]`).
pub type Page = Vec<String>;

// ---- pure text helpers -------------------------------------------------------------------

/// Python's `str.isspace()` for one character: Unicode White_Space plus the
/// ASCII information separators U+001C..U+001F, which `str.split()` also splits on.
fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Python's `str.split()` with no arguments.
fn py_split(text: &str) -> impl Iterator<Item = &str> {
    text.split(py_isspace).filter(|w| !w.is_empty())
}

/// Greedy word wrap of the whitespace-normalised text. A word longer than
/// `cols` is hard-split. No line exceeds `cols`, none has leading/trailing
/// spaces, "" -> []. Prefix-stable: appending words never changes earlier lines.
/// Lengths are counted in characters (code points), as Python's `len`.
pub fn wrap(text: &str, cols: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_len = 0usize;
    for w in py_split(text) {
        let mut word: Vec<char> = w.chars().collect();
        while word.len() > cols {
            if !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
                cur_len = 0;
            }
            lines.push(word[..cols].iter().collect());
            word.drain(..cols);
        }
        let word_s: String = word.iter().collect();
        if cur.is_empty() {
            cur = word_s;
            cur_len = word.len();
        } else if cur_len + 1 + word.len() <= cols {
            cur.push(' ');
            cur.push_str(&word_s);
            cur_len += 1 + word.len();
        } else {
            lines.push(std::mem::replace(&mut cur, word_s));
            cur_len = word.len();
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

/// The part of a streaming text that ends on a whole word: everything up to
/// and including the last whitespace ("Ten past thr" -> "Ten past "); the whole
/// text when final.
pub fn complete_prefix(text: &str, r#final: bool) -> &str {
    if r#final {
        return text;
    }
    match text.char_indices().rev().find(|&(_, c)| py_isspace(c)) {
        Some((i, c)) => &text[..i + c.len_utf8()],
        None => "",
    }
}

/// Split lines into pages of `per_page` lines; the last page may be shorter.
pub fn paginate(lines: &[String], per_page: usize) -> Vec<Page> {
    lines.chunks(per_page).map(|c| c.to_vec()).collect()
}

/// `max(lo, min(hi, x))`: unlike `f64::clamp`, never panics when `lo > hi` (lo wins).
fn clamp(x: f64, lo: f64, hi: f64) -> f64 {
    lo.max(hi.min(x))
}

/// How long a page of `chars` characters stays up, from the moment it is shown.
pub fn page_hold_secs(chars: usize, cfg: &PagerConfig, first: bool, last: bool) -> f64 {
    let secs = chars as f64 / cfg.read_cps + if first { cfg.first_page_lead_secs } else { 0.0 };
    if last {
        return clamp(secs + cfg.last_page_extra_secs, cfg.last_page_min_secs, cfg.last_page_max_secs);
    }
    clamp(secs, cfg.min_hold_secs, cfg.max_hold_secs)
}

/// The extra session instructions for captions mode (voice_agent).
pub fn caption_instructions(cfg: &PagerConfig) -> String {
    let per_page = cfg.lines * cfg.cols;
    format!(
        "\nYour replies are not spoken: they appear as text on your own small screen, one page at a time. \
         A page is\n{lines} lines of {cols} characters (about {per_page} characters) and stays up \
         long enough to read; one page is ideal, two\nis the most. Keep every reply under \
         {budget} characters: plain words, short sentences, no emoji, no lists,\n\
         no line breaks.",
        lines = cfg.lines,
        cols = cfg.cols,
        budget = cfg.reply_budget_chars,
    )
}

// ---- events ---------------------------------------------------------------------------------

/// Put a page on the robot's screen.
#[derive(Debug, Clone, PartialEq)]
pub struct ShowPage {
    /// index within its reply
    pub page: usize,
    /// page count of the reply, 0 while the reply is still streaming
    pub of: usize,
    pub lines: Page,
    /// the host's planned hold from now; the board self-clears at hold + grace
    pub hold_ms: i64,
    /// true on a page turn (one talk chirp per page), false on a refill
    pub chirp: bool,
    /// last page of a final reply
    pub r#final: bool,
}

impl ShowPage {
    pub fn to_wire(&self) -> Value {
        json!({
            "cmd": "caption",
            "page": self.page,
            "of": self.of,
            "lines": self.lines,
            "hold_ms": self.hold_ms,
            "chirp": self.chirp,
            "final": self.r#final,
        })
    }
}

/// Clear the caption band.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Clear;

impl Clear {
    pub fn to_wire(&self) -> Value {
        json!({"cmd": "caption", "clear": true})
    }
}

/// Python: `Event = Union[ShowPage, Clear]`.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    ShowPage(ShowPage),
    Clear(Clear),
}

impl Event {
    pub fn to_wire(&self) -> Value {
        match self {
            Event::ShowPage(s) => s.to_wire(),
            Event::Clear(c) => c.to_wire(),
        }
    }

    pub fn as_show(&self) -> Option<&ShowPage> {
        match self {
            Event::ShowPage(s) => Some(s),
            Event::Clear(_) => None,
        }
    }

    pub fn is_clear(&self) -> bool {
        matches!(self, Event::Clear(_))
    }
}

// ---- the pager --------------------------------------------------------------------------------

/// One reply's text and page timings (Python: `_Reply`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Reply {
    pub text: String,
    pub r#final: bool,
    pub pages: Vec<Page>,
    pub t_shown: Vec<Option<f64>>,
    pub t_frozen: Vec<Option<f64>>,
    pub overflow_warned: bool,
}

/// See the module docs. `poll(now)` drives everything; the caller polls at
/// its tick and after every `update`.
#[derive(Debug, Clone)]
pub struct CaptionPager {
    pub cfg: PagerConfig,
    pub replies: Vec<Reply>,
    /// (reply index, page index) of the page that should be up
    pub cur: (usize, usize),
    /// false while cur waits for its first line (no blank page)
    cur_shown: bool,
    pub last_refill_at: f64,
    pub last_sent: Option<ShowPage>,
}

impl Default for CaptionPager {
    fn default() -> Self {
        Self::new(PagerConfig::default())
    }
}

impl CaptionPager {
    pub fn new(cfg: PagerConfig) -> Self {
        Self {
            cfg,
            replies: Vec::new(),
            cur: (0, 0),
            cur_shown: false,
            last_refill_at: f64::NEG_INFINITY,
            last_sent: None,
        }
    }

    // -- public state --
    pub fn busy(&self) -> bool {
        self.last_sent.is_some()
    }

    pub fn visible(&self) -> Option<&ShowPage> {
        self.last_sent.as_ref()
    }

    // -- input --

    /// response.created. Busy: the new reply queues behind the page on screen
    /// (whose last-page hold is thereby demoted to the normal formula). Idle: nothing
    /// happens until text arrives. An empty reply that never got text is reused.
    pub fn begin_reply(&mut self, _now: f64) {
        if let Some(last) = self.replies.last() {
            if last.text.is_empty() && !last.r#final {
                return;
            }
        }
        self.replies.push(Reply::default());
        if self.replies.len() == 1 {
            self.cur = (0, 0);
            self.cur_shown = false;
        }
    }

    /// The full text of the current (newest) reply so far.
    pub fn update(&mut self, now: f64, text: &str, r#final: bool) {
        if self.replies.is_empty() {
            if text.is_empty() {
                return;
            }
            self.replies = vec![Reply::default()];
            self.cur = (0, 0);
            self.cur_shown = false;
        }
        let cfg = self.cfg;
        let reply = self.replies.last_mut().expect("replies is not empty");
        if reply.r#final && !r#final {
            return; // a stray delta after the done: keep the final text
        }
        reply.text = text.to_string();
        reply.r#final = r#final;
        repaginate(&cfg, reply, now);
        if r#final && reply.pages.is_empty() {
            self.drop_empty_tail(); // a reply with nothing to show (tool call only)
        }
    }

    /// response.done with no text.done seen: finalise whatever the reply has.
    pub fn end_reply(&mut self, now: f64) {
        if let Some(last) = self.replies.last() {
            if !last.r#final {
                let text = last.text.clone();
                self.update(now, &text, true);
            }
        }
    }

    /// Barge-in / hush: drop everything; a Clear iff something is on screen.
    pub fn reset(&mut self, _now: f64) -> Vec<Event> {
        let events = if self.last_sent.is_some() { vec![Event::Clear(Clear)] } else { Vec::new() };
        self.drop_all();
        events
    }

    // -- output --

    /// Refills, page turns and the final clear that are due at `now`, in order.
    pub fn poll(&mut self, now: f64) -> Vec<Event> {
        let mut events = Vec::new();
        while let Some(e) = self.step(now) {
            let clear = e.is_clear();
            events.push(e);
            if clear {
                break;
            }
        }
        events
    }

    // -- internals --

    fn drop_all(&mut self) {
        self.replies.clear();
        self.cur = (0, 0);
        self.cur_shown = false;
        self.last_refill_at = f64::NEG_INFINITY;
        self.last_sent = None;
    }

    /// The newest reply is final with no pages: forget it. If it was the page
    /// the pager was waiting to turn to, the page on screen becomes last again.
    fn drop_empty_tail(&mut self) {
        let r = self.replies.len() - 1;
        self.replies.pop();
        if self.cur.0 != r {
            return;
        }
        if r == 0 {
            self.drop_all();
        } else {
            // cur only reaches reply r by turning from the last page of reply r - 1,
            // so that reply has at least one page.
            self.cur = (r - 1, self.replies[r - 1].pages.len().saturating_sub(1));
            self.cur_shown = true;
        }
    }

    /// Whether page `p` of reply `r` is the last page of the last, final reply
    /// (Python: `_is_last_overall`).
    pub fn is_last_overall(&self, r: usize, p: usize) -> bool {
        let reply = &self.replies[r];
        r == self.replies.len() - 1 && reply.r#final && p + 1 == reply.pages.len()
    }

    fn chars(&self, r: usize, p: usize) -> usize {
        self.replies[r].pages[p].iter().map(|l| l.chars().count()).sum()
    }

    fn deadline(&self, r: usize, p: usize) -> f64 {
        let reply = &self.replies[r];
        let hold = page_hold_secs(self.chars(r, p), &self.cfg, p == 0, false);
        let shown = reply.t_shown[p].expect("deadline of a page never shown");
        let frozen = reply.t_frozen[p].expect("deadline of a page not frozen");
        (shown + hold).max(frozen + self.cfg.freeze_dwell_secs)
    }

    /// When the last page of the last reply clears (Python: `_clear_at`).
    pub fn clear_at(&self, r: usize, p: usize) -> f64 {
        let reply = &self.replies[r];
        let hold = page_hold_secs(self.chars(r, p), &self.cfg, p == 0, true);
        let shown = reply.t_shown[p].expect("clear time of a page never shown");
        let frozen = reply.t_frozen[p].expect("clear time of a page not frozen");
        (shown + hold).max(frozen + self.cfg.last_page_extra_secs)
    }

    fn page_changed(&self, r: usize, p: usize) -> bool {
        let Some(sent) = &self.last_sent else {
            return true;
        };
        let reply = &self.replies[r];
        sent.lines != reply.pages[p] || sent.r#final != final_flag(reply, p) || sent.of != of(reply)
    }

    fn show(&mut self, r: usize, p: usize, now: f64, chirp: bool) -> ShowPage {
        let reply = &self.replies[r];
        let shown = reply.t_shown[p].expect("show of a page with no t_shown");
        let planned = if reply.t_frozen[p].is_some() {
            if self.is_last_overall(r, p) {
                self.clear_at(r, p)
            } else {
                self.deadline(r, p)
            }
        } else {
            shown + page_hold_secs(self.chars(r, p), &self.cfg, p == 0, false)
        };
        let ev = ShowPage {
            page: p,
            of: of(reply),
            lines: reply.pages[p].clone(),
            // Python's round(): half to even
            hold_ms: (1000.0 * (planned - now).max(0.0)).round_ties_even() as i64,
            chirp,
            r#final: final_flag(reply, p),
        };
        self.last_sent = Some(ev.clone());
        self.last_refill_at = now;
        self.cur_shown = true;
        ev
    }

    fn step(&mut self, now: f64) -> Option<Event> {
        loop {
            if self.replies.is_empty() {
                return None;
            }
            let (r, p) = self.cur;
            if !self.cur_shown {
                // first show of this page, once it has a line
                if p < self.replies[r].pages.len() {
                    self.replies[r].t_shown[p] = Some(now);
                    return Some(Event::ShowPage(self.show(r, p, now, true)));
                }
                return None;
            }
            let frozen = self.replies[r].t_frozen[p].is_some();
            if self.page_changed(r, p)
                && (frozen || now - self.last_refill_at >= self.cfg.refill_interval_secs - 1e-9)
            {
                // a refill that freezes the page skips the throttle
                return Some(Event::ShowPage(self.show(r, p, now, false)));
            }
            if !frozen {
                return None;
            }
            if self.is_last_overall(r, p) {
                if now >= self.clear_at(r, p) {
                    self.drop_all();
                    return Some(Event::Clear(Clear));
                }
                return None;
            }
            if now < self.deadline(r, p) {
                return None;
            }
            // turn the page: next page of this reply, else page 0 of the next reply
            if p + 1 < self.replies[r].pages.len() {
                self.cur = (r, p + 1);
            } else if r + 1 < self.replies.len() {
                self.cur = (r + 1, 0);
            } else {
                return None; // the reply is still streaming and its next page is empty
            }
            self.cur_shown = false;
            // loop: shows it now if it has content, else waits
        }
    }
}

fn of(reply: &Reply) -> usize {
    if reply.r#final {
        reply.pages.len()
    } else {
        0
    }
}

fn final_flag(reply: &Reply, p: usize) -> bool {
    reply.r#final && p + 1 == reply.pages.len()
}

fn repaginate(cfg: &PagerConfig, reply: &mut Reply, now: f64) {
    let mut pages = paginate(&wrap(complete_prefix(&reply.text, reply.r#final), cfg.cols), cfg.lines);
    if pages.len() > cfg.max_pages {
        pages.truncate(cfg.max_pages);
        if let Some(tail) = pages.last_mut().and_then(|last| last.last_mut()) {
            let n = tail.chars().count();
            let kept: String =
                if n < cfg.cols { tail.clone() } else { tail.chars().take(cfg.cols.saturating_sub(1)).collect() };
            *tail = kept + ELLIPSIS;
        }
        if !reply.overflow_warned {
            reply.overflow_warned = true;
            tracing::warn!("captions: reply longer than {} pages; the tail is dropped", cfg.max_pages);
        }
    }
    reply.pages = pages;
    while reply.t_shown.len() < reply.pages.len() {
        reply.t_shown.push(None);
        reply.t_frozen.push(None);
    }
    let n = reply.pages.len();
    for i in 0..n {
        if reply.t_frozen[i].is_none() && (i + 1 < n || reply.r#final) {
            reply.t_frozen[i] = Some(now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn py_split_matches_python_whitespace() {
        assert_eq!(py_split("a\u{1c}b\u{a0}c\u{3000}d  e\n").collect::<Vec<_>>(), ["a", "b", "c", "d", "e"]);
        assert_eq!(py_split("a\u{200b}b").collect::<Vec<_>>(), ["a\u{200b}b"]);
    }

    #[test]
    fn wrap_counts_characters_not_bytes() {
        let lines = wrap("Spotify’s open—and ready ééééééééééééééééééé", 17);
        assert!(lines.iter().all(|l| l.chars().count() <= 17));
        assert_eq!(lines[0], "Spotify’s");
        assert_eq!(lines.last().unwrap(), "éé");
    }

    #[test]
    fn complete_prefix_multibyte_whitespace() {
        assert_eq!(complete_prefix("Ten\u{3000}pa", false), "Ten\u{3000}");
    }

    #[test]
    fn clamp_prefers_lo_like_python() {
        assert_eq!(clamp(5.0, 6.0, 4.0), 6.0);
    }
}
