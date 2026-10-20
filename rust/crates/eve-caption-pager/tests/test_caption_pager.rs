//! Port of bridge/tests/test_caption_pager.py: the pager with a fake clock:
//! wrapping, page holds, streaming page turns, reply chaining, barge-in,
//! overflow, and the geometry shared with the firmware. Pure module, no async.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use eve_caption_pager::{
    caption_instructions, complete_prefix, page_hold_secs, paginate, wrap, CaptionPager, Clear, Event, Page,
    PagerConfig, ShowPage,
};

fn cfg() -> PagerConfig {
    PagerConfig::default()
}

fn clamp(x: f64, lo: f64, hi: f64) -> f64 {
    lo.max(hi.min(x))
}

fn chars(page: &[String]) -> usize {
    page.iter().map(|l| l.chars().count()).sum()
}

fn approx(a: f64, b: f64, abs: f64) -> bool {
    // pytest.approx: max(rel 1e-6 * |b|, abs)
    (a - b).abs() <= abs.max(1e-6 * b.abs())
}

fn round6(x: f64) -> f64 {
    (x * 1e6).round_ties_even() / 1e6
}

fn py_split(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_string).collect()
}

/// Character slice `s[..n]` by code points (Python `s[:n]`).
fn prefix(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn pages_of(text: &str, fin: bool) -> Vec<Page> {
    paginate(&wrap(complete_prefix(text, fin), 17), 4)
}

/// Deterministic test RNG (SplitMix64) standing in for Python's `random.Random(seed)`:
/// the tests assert invariants that hold for any seed.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn random(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn randint(&mut self, a: usize, b: usize) -> usize {
        a + (self.next_u64() % (b - a + 1) as u64) as usize
    }
    fn uniform(&mut self, a: f64, b: f64) -> f64 {
        a + (b - a) * self.random()
    }
    fn word(&mut self, alphabet: &str, k: usize) -> String {
        let cs: Vec<char> = alphabet.chars().collect();
        (0..k).map(|_| cs[self.randint(0, cs.len() - 1)]).collect()
    }
}

/// Feeds a reply into a pager at `cps` and records every event with its time.
struct Stream<'a> {
    pager: CaptionPager,
    text: String,
    cps: f64,
    tick: f64,
    delta: usize,
    jitter: Option<&'a mut Rng>,
    events: Vec<(f64, Event)>,
    updates: Vec<(f64, String, bool)>,
    t: f64,
}

impl<'a> Stream<'a> {
    fn new(pager: CaptionPager, text: &str, cps: f64) -> Self {
        Self {
            pager,
            text: text.to_string(),
            cps,
            tick: 0.05,
            delta: 4,
            jitter: None,
            events: Vec::new(),
            updates: Vec::new(),
            t: 0.0,
        }
    }

    fn poll(&mut self) {
        for e in self.pager.poll(self.t) {
            self.events.push((self.t, e));
        }
    }

    fn run(&mut self) {
        self.run_for(600.0)
    }

    fn run_for(&mut self, max_secs: f64) {
        let total = self.text.chars().count();
        self.pager.begin_reply(self.t);
        self.poll();
        let mut sent = 0usize;
        let mut next_delta_at = self.t;
        let mut final_at: Option<f64> = None;
        let end = self.t + max_secs;
        while self.t < end {
            while sent < total && self.t >= next_delta_at {
                let n = match self.jitter.as_deref_mut() {
                    None => self.delta,
                    Some(rng) => rng.randint(1, 2 * self.delta),
                };
                sent = total.min(sent + n);
                let fin = sent == total;
                let part = prefix(&self.text, sent);
                self.pager.update(self.t, &part, fin);
                self.updates.push((self.t, part, fin));
                if fin {
                    final_at = Some(self.t);
                }
                let gap = n as f64 / self.cps;
                next_delta_at += match self.jitter.as_deref_mut() {
                    None => gap,
                    Some(rng) => gap * rng.uniform(0.5, 1.5),
                };
            }
            self.poll();
            if final_at.is_some() && !self.pager.busy() && self.pager.replies.is_empty() {
                return;
            }
            self.t = round6(self.t + self.tick);
        }
        panic!("pager never went idle");
    }

    fn shows(&self) -> Vec<(f64, ShowPage)> {
        self.events.iter().filter_map(|(t, e)| e.as_show().map(|s| (*t, s.clone()))).collect()
    }

    fn clears(&self) -> Vec<f64> {
        self.events.iter().filter(|(_, e)| e.is_clear()).map(|(t, _)| *t).collect()
    }

    /// When page 0 froze: the first update after which a second page had content.
    fn t_frozen0(&self) -> f64 {
        self.updates.iter().find(|(_, text, fin)| pages_of(text, *fin).len() > 1).map(|(t, _, _)| *t).unwrap()
    }
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

// ---- pure helpers ------------------------------------------------------------------------------

#[test]
fn test_wrap_never_exceeds_cols_and_keeps_word_order() {
    let plain = "Nice,  Spotify's open\nand ready.\n\nGoodbye,   eve. What time is it now, exactly?";
    let lines = wrap(plain, 17);
    assert!(!lines.is_empty() && lines.iter().all(|l| (1..=17).contains(&l.chars().count())));
    assert!(lines.iter().all(|l| l == l.trim()));
    assert_eq!(py_split(&lines.join(" ")), py_split(plain));
    let url = "see https://example.com/a/very/long/path"; // a 30-char token: hard-split
    let lines = wrap(url, 17);
    assert!(lines.iter().all(|l| (1..=17).contains(&l.chars().count())));
    assert_eq!(py_split(&lines.concat()).concat(), py_split(url).concat());
    assert!(lines[0] == "see" && lines[1] == "https://example.c");
    assert!(wrap("", 17).is_empty() && wrap("   \n ", 17).is_empty());
}

#[test]
fn test_wrap_is_prefix_stable() {
    let mut rng = Rng(7);
    for _ in 0..200 {
        let n_words = rng.randint(1, 40);
        let words: Vec<String> = (0..n_words)
            .map(|_| {
                let k = rng.randint(1, 12);
                rng.word("abcdefghijklmnop", k)
            })
            .collect();
        let full = wrap(&words.join(" "), 17);
        for k in 1..=words.len() {
            let part = wrap(&words[..k].join(" "), 17);
            let n = part.len();
            assert_eq!(part[..n - 1], full[..n - 1]);
            assert!(full[n - 1].starts_with(&part[n - 1]));
        }
    }
}

#[test]
fn test_complete_prefix() {
    assert_eq!(complete_prefix("Ten past thr", false), "Ten past ");
    assert_eq!(complete_prefix("Ten past thr", true), "Ten past thr");
    assert_eq!(complete_prefix("a ", false), "a ");
    assert!(complete_prefix("", false).is_empty() && complete_prefix("", true).is_empty());
    assert_eq!(complete_prefix("word", false), "");
}

#[test]
fn test_paginate() {
    let seven: Vec<String> = (0..7).map(|i| format!("l{i}")).collect();
    let pages = paginate(&seven, 4);
    assert_eq!(pages.iter().map(Vec::len).collect::<Vec<_>>(), [4, 3]);
    assert_eq!(pages[0], s(&["l0", "l1", "l2", "l3"]));
    assert!(paginate(&[], 4).is_empty());
    assert_eq!(paginate(&seven[..4], 4), vec![seven[..4].to_vec()]);
}

#[test]
fn test_page_hold_table() {
    let table: [(usize, bool, bool, f64); 8] = [
        (68, true, false, 68.0 / 12.0 + 0.8),       // 6.47
        (10, true, false, 2.0),                     // floor
        (200, true, false, 9.0),                    // cap
        (47, true, true, 47.0 / 12.0 + 0.8 + 3.0),  // 7.72
        (5, true, true, 6.0),
        (100, true, true, 10.0),
        (15, true, true, 6.0),                      // "Ten past three."
        (30, false, false, 2.5),                    // no lead on a later page
    ];
    for (chars, first, last, expect) in table {
        assert!(approx(page_hold_secs(chars, &cfg(), first, last), expect, 1e-12), "{chars} {first} {last}");
        assert_eq!(page_hold_secs(68, &PagerConfig::with_read_cps(8.0), true, false), 9.0);
    }
}

// ---- streaming ---------------------------------------------------------------------------------

const REPLY_110: &str = "Nice, Spotify is open and ready for you now. The playlist you asked for is queued up. \
                         Goodbye, eve, catch you.";

#[test]
fn test_stream_at_40cps_pages_and_holds() {
    assert_eq!(REPLY_110.chars().count(), 110);
    let mut st = Stream::new(CaptionPager::default(), REPLY_110, 40.0);
    st.run();
    let shows = st.shows();
    let (t0, first) = shows[0].clone();
    assert!(first.page == 0 && first.of == 0 && first.chirp && !first.r#final);
    assert!(!first.lines.is_empty() && approx(t0, 0.1, 1e-12)); // first complete word ("Nice,") after the first delta
    let pages = paginate(&wrap(REPLY_110, 17), 4);
    assert_eq!(pages.len(), 2);
    // page 0 froze when page 1 first had content
    let t_frozen0 = st.t_frozen0();
    let refills: Vec<&(f64, ShowPage)> = shows.iter().filter(|(_, e)| e.page == 0 && !e.chirp).collect();
    assert!(!refills.is_empty());
    for w in refills.windows(2) {
        let (ta, tb) = (w[0].0, w[1].0);
        assert!(tb - ta >= 0.15 - 1e-9 || tb == t_frozen0); // the freeze refill goes out at once
    }
    assert_eq!(refills.last().unwrap().1.lines, pages[0]);
    let turn: Vec<&(f64, ShowPage)> = shows.iter().filter(|(_, e)| e.page == 1).collect();
    assert_eq!(turn.len(), 1);
    let (t1, second) = turn[0].clone();
    let expect_t1 = (t0 + clamp(chars(&pages[0]) as f64 / 12.0 + 0.8, 2.0, 9.0)).max(t_frozen0 + 2.0);
    assert!(approx(t1, expect_t1, 0.05 + 1e-9) && t1 >= expect_t1 - 1e-9);
    let hold1 = clamp(chars(&pages[1]) as f64 / 12.0 + 3.0, 6.0, 10.0);
    assert_eq!(
        second,
        ShowPage {
            page: 1,
            of: 2,
            lines: pages[1].clone(),
            hold_ms: (1000.0 * hold1).round_ties_even() as i64,
            chirp: true,
            r#final: true,
        }
    );
    let clears = st.clears();
    assert!(clears.len() == 1 && approx(clears[0], t1 + hold1, 0.05 + 1e-9) && clears[0] >= t1 + hold1 - 1e-9);
    assert!(!st.pager.busy());
    for (_, e) in &st.events {
        let w = e.to_wire();
        assert_eq!(w["cmd"], "caption");
        if let Some(lines) = w.get("lines") {
            let lines = lines.as_array().unwrap();
            assert!(lines.len() <= 4 && lines.iter().all(|l| l.as_str().unwrap().chars().count() <= 17));
        }
    }
}

#[test]
fn test_slow_stream_respects_freeze_dwell() {
    let mut st = Stream::new(CaptionPager::default(), REPLY_110, 5.0);
    st.run();
    let shows = st.shows();
    let t0 = shows[0].0;
    let t_frozen0 = st.t_frozen0();
    let t1 = shows.iter().find(|(_, e)| e.page == 1).unwrap().0;
    assert!(t_frozen0 > t0 + 6.47); // the hold passed long before the page froze
    assert!(t1 >= t_frozen0 + 2.0 - 1e-9 && t1 < t_frozen0 + 2.0 + 0.05 + 1e-9);
}

#[test]
fn test_dwell_invariants_fuzz() {
    use std::collections::HashMap;
    let mut rng = Rng(2026);
    for _ in 0..200 {
        let words: Vec<String> = (0..400)
            .map(|_| {
                let k = rng.randint(1, 9);
                rng.word("abcdefghijklmnopqrstuvwxyz", k)
            })
            .collect();
        let mut text = String::new();
        // the bound is re-drawn each iteration, as in the Python test
        while text.chars().count() < rng.randint(20, 300) {
            let idx = text.split_whitespace().count();
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(&words[idx]);
        }
        let cps = rng.uniform(5.0, 80.0);
        let mut st = Stream::new(CaptionPager::default(), &text, cps);
        st.jitter = Some(&mut rng);
        st.run();
        let shows = st.shows();
        let pages = paginate(&wrap(&text, 17), 4);
        let overflow = pages.len() > 6;
        let mut frozen_at: HashMap<usize, f64> = HashMap::new();
        for (t, txt, fin) in &st.updates {
            let ps = pages_of(txt, *fin);
            for i in 0..ps.len() {
                if !frozen_at.contains_key(&i) && (i + 1 < ps.len() || *fin) {
                    frozen_at.insert(i, *t);
                }
            }
        }
        let mut shown_at: HashMap<usize, f64> = HashMap::new();
        let mut last_lines: HashMap<usize, Page> = HashMap::new();
        let mut seen_pages: Vec<usize> = Vec::new();
        for (t, e) in &shows {
            if e.chirp {
                assert!(!shown_at.contains_key(&e.page));
                shown_at.insert(e.page, *t);
                seen_pages.push(e.page);
            } else if !overflow {
                let prev = &last_lines[&e.page];
                assert!(e.lines.len() >= prev.len());
                for (a, b) in prev[..prev.len() - 1].iter().zip(e.lines.iter()) {
                    assert_eq!(a, b); // a full line never changes
                }
                assert!(e.lines[prev.len() - 1].starts_with(prev.last().unwrap().as_str()));
            }
            last_lines.insert(e.page, e.lines.clone());
        }
        let mut sorted = seen_pages.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(seen_pages, sorted);
        let n = seen_pages.len();
        assert_eq!(n, pages.len().min(6));
        for i in 0..n - 1 {
            let visible = shown_at[&(i + 1)] - shown_at[&i];
            assert!(visible >= (2.0f64).max(chars(&last_lines[&i]) as f64 / 12.0) - 1e-9);
            assert!(shown_at[&(i + 1)] - frozen_at[&i] >= 2.0 - 1e-9);
        }
        let clear = st.clears()[0];
        let last = n - 1;
        assert!(clear - shown_at[&last] >= 6.0 - 1e-9);
        assert!(clear - shown_at[&last].max(frozen_at[&last]) <= 10.0 + 0.11); // one tick + jitter of the poll
        if !overflow {
            let joined: Vec<String> = (0..n).map(|i| last_lines[&i].join(" ")).collect();
            assert_eq!(py_split(&joined.join(" ")), py_split(&text));
        }
    }
}

#[test]
fn test_logged_replies_regression() {
    // Replies from the daemon log of 2026-09-06 (there was no 213-char reply in
    // it; the longest logged reply is this 130-char one).
    let logged = [
        "Nice, Spotify’s open and ready. Goodbye, eve.",
        "Hey there! Thanks for the welcome. I’m here, ready to help with anything you’ve got—just let me know \
         what you fancy tackling next.",
    ];
    assert_eq!(logged[1].chars().count(), 130);
    for text in logged {
        let mut st = Stream::new(CaptionPager::default(), text, 40.0);
        st.run();
        let shows = st.shows();
        // every line is on screen at least max(1.0, len/15) s: a line first appears
        // in some show and stays until the page turns / the clear
        let pages = paginate(&wrap(text, 17), 4);
        let mut appeared: Vec<((usize, usize), f64)> = Vec::new();
        let mut gone: std::collections::HashMap<usize, f64> = Default::default();
        for (t, e) in &shows {
            for i in 0..e.lines.len() {
                if !appeared.iter().any(|(k, _)| *k == (e.page, i)) {
                    appeared.push(((e.page, i), *t));
                }
            }
            if e.chirp && e.page > 0 {
                gone.insert(e.page - 1, *t);
            }
        }
        gone.insert(pages.len() - 1, st.clears()[0]);
        for ((page, i), t_in) in appeared {
            let line = &pages[page][i];
            assert!(gone[&page] - t_in >= (1.0f64).max(line.chars().count() as f64 / 15.0) - 1e-9);
        }
        let joined: Vec<String> = pages.iter().map(|p| p.join(" ")).collect();
        assert_eq!(py_split(&joined.join(" ")), py_split(text));
        assert!(pages.iter().flatten().all(|l| l.chars().count() <= 17));
    }
}

#[test]
fn test_reply_chaining_demotes_last_page() {
    let mut pager = CaptionPager::default();
    let mut ev: Vec<(f64, Event)> = Vec::new();
    let poll = |pager: &mut CaptionPager, ev: &mut Vec<(f64, Event)>, t: f64| {
        for e in pager.poll(t) {
            ev.push((t, e));
        }
    };

    let a = "Nice, Spotify is ready.";
    pager.begin_reply(0.0);
    pager.update(0.0, a, true);
    poll(&mut pager, &mut ev, 0.0);
    let chars_a = chars(&paginate(&wrap(a, 17), 4)[0]);
    let first = ev[0].1.as_show().unwrap();
    assert!(first.r#final && first.hold_ms == 6000);
    pager.begin_reply(0.5);
    poll(&mut pager, &mut ev, 0.5);
    let b = "Sure, playing it now.";
    pager.update(1.0, b, true);
    poll(&mut pager, &mut ev, 1.0);
    let expect = (0.0 + clamp(chars_a as f64 / 12.0 + 0.8, 2.0, 9.0)).max(0.0 + 2.0);
    let mut t = 1.0;
    while t < expect - 1e-9 {
        t = round6(t + 0.05);
        poll(&mut pager, &mut ev, t);
        if t < expect - 1e-9 {
            assert_eq!(ev.len(), 1, "B's page appeared before A's demoted hold ran out");
        }
    }
    let shows: Vec<(f64, ShowPage)> = ev.iter().filter_map(|(t, e)| e.as_show().map(|s| (*t, s.clone()))).collect();
    assert_eq!(shows.len(), 2);
    let (tb, page_b) = shows[1].clone();
    assert!(approx(tb, expect, 0.05 + 1e-9));
    assert!(page_b.page == 0 && page_b.chirp && page_b.r#final && page_b.of == 1);
    assert!(!ev.iter().any(|(_, e)| e.is_clear()));
    while t < tb + 6.0 + 0.1 {
        t = round6(t + 0.05);
        poll(&mut pager, &mut ev, t);
    }
    let clears: Vec<f64> = ev.iter().filter(|(_, e)| e.is_clear()).map(|(t, _)| *t).collect();
    assert!(clears.len() == 1 && approx(clears[0], tb + 6.0, 0.05 + 1e-9));
    assert!(!pager.busy());
}

#[test]
fn test_reset_drops_queue_and_clears() {
    let mut pager = CaptionPager::default();
    assert_eq!(pager.reset(0.0), vec![]);
    pager.begin_reply(0.0);
    pager.update(0.0, "Ten past three.", true);
    let ev = pager.poll(0.0);
    assert!(ev.len() == 1 && ev[0].as_show().is_some());
    pager.begin_reply(0.2);
    pager.update(0.3, "And a queued reply.", true);
    assert_eq!(pager.poll(1.0), vec![]);
    assert_eq!(pager.reset(1.0), vec![Event::Clear(Clear)]);
    assert!(!pager.busy() && pager.poll(5.0).is_empty());
    pager.update(6.0, "Fresh start ", false);
    let ev = pager.poll(6.0);
    assert_eq!(ev.len(), 1);
    let show = ev[0].as_show().unwrap();
    assert!(show.page == 0 && show.chirp);
}

/// Collects formatted tracing output (stands in for pytest's caplog).
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn test_overflow_truncates_with_ellipsis() {
    let text: String = (0..72).map(|i| format!("word{i}")).collect::<Vec<_>>().join(" "); // ~ 500 chars
    assert!((480..=520).contains(&text.len()));
    let mut pager = CaptionPager::default();
    let cap = Capture::default();
    let writer = cap.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        pager.begin_reply(0.0);
        let mut k = 100;
        while k < text.len() {
            pager.update(k as f64 / 100.0, &text[..k], false);
            k += 100;
        }
        pager.update(6.0, &text, true);
    });
    let pages = &pager.replies[0].pages;
    assert_eq!(pages.len(), 6);
    let tail = pages.last().unwrap().last().unwrap();
    assert!(tail.ends_with('…') && tail.chars().count() <= 17);
    let log = String::from_utf8(cap.0.lock().unwrap().clone()).unwrap();
    assert_eq!(log.matches("longer than 6 pages").count(), 1);
}

#[test]
fn test_geometry_matches_firmware() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let src = repo.join("firmware/claude_pet_stackchan/src");
    let main = std::fs::read_to_string(src.join("main.cpp")).unwrap();
    let data = std::fs::read_to_string(src.join("data.h")).unwrap();
    let int_after = |hay: &str, key: &str| -> usize {
        let i = hay.find(key).unwrap_or_else(|| panic!("{key} not found")) + key.len();
        hay[i..].chars().take_while(char::is_ascii_digit).collect::<String>().parse().unwrap()
    };
    let cols = int_after(&main, "CAP_COLS = ");
    let lines = int_after(&main, "CAP_LINES = ");
    let max_lines = int_after(&data, "CAP_MAX_LINES = ");
    let max_cols = int_after(&data, "CAP_MAX_LINES = 6, CAP_MAX_COLS = ");
    assert_eq!(max_lines, 6);
    let c = cfg();
    assert_eq!((c.cols, c.lines), (cols, lines));
    assert!(c.lines <= max_lines && c.cols <= max_cols);
}

#[test]
fn test_caption_instructions_state_the_geometry() {
    let text = caption_instructions(&cfg());
    assert!(text.contains("4 lines of 17 characters") && text.contains("68") && text.contains("under 100 characters"));
    assert!(!text.contains("three lines"));
}

#[test]
fn test_caption_instructions_verbatim() {
    assert_eq!(
        caption_instructions(&cfg()),
        "\nYour replies are not spoken: they appear as text on your own small screen, one page at a time. A page is\n\
         4 lines of 17 characters (about 68 characters) and stays up long enough to read; one page is ideal, two\n\
         is the most. Keep every reply under 100 characters: plain words, short sentences, no emoji, no lists,\n\
         no line breaks."
    );
}

#[test]
fn test_tool_only_response_while_a_page_is_held_does_not_strand_it() {
    // response.created for a tool-only response queues an empty reply behind the page on
    // screen; end_reply (response.done with no text) must drop it so the page on screen is the
    // last page again and clears on time — otherwise it sits there until barge-in.
    let mut pg = CaptionPager::default();
    pg.begin_reply(0.0);
    pg.update(0.1, "Ten past three.", true);
    let shows = pg.poll(0.1);
    assert!(shows.last().unwrap().as_show().unwrap().r#final);
    let clear_alone = pg.clear_at(0, 0);
    pg.begin_reply(0.5); // a second response starts (tool call only)
    assert!(!pg.is_last_overall(0, 0)); // the held page is no longer "last"
    pg.end_reply(0.6); // ... and ends with no text
    assert!(pg.replies.len() == 1 && pg.is_last_overall(0, 0) && pg.clear_at(0, 0) == clear_alone);
    let events = pg.poll(clear_alone + 0.01);
    assert!(events.iter().any(Event::is_clear));
    assert!(!pg.busy());
}

#[test]
fn test_last_page_clear_waits_for_the_freeze_dwell() {
    // clear(last) = max(t_shown + hold_last, t_frozen + last_page_extra): a page that kept
    // filling until late must still be readable for last_page_extra after its last change.
    let c = PagerConfig::default();
    let mut pg = CaptionPager::new(c);
    pg.begin_reply(0.0);
    pg.update(0.1, "One", false);
    pg.poll(0.1); // shown at 0.1
    pg.update(5.0, "One two three four five six", true); // final text lands late, still one page
    pg.poll(5.0);
    let deadline = pg.clear_at(0, 0);
    // invariants: never before the final text has been readable for last_page_extra, and never
    // before the hold a page of that length earns from the moment it was last (re)shown
    assert!(deadline >= 5.0 + c.last_page_extra_secs - 1e-9);
    assert!(deadline >= 5.0 + c.last_page_min_secs - 1e-9);
    assert!(!pg.poll(deadline - 0.05).iter().any(Event::is_clear));
    assert!(pg.poll(deadline + 0.01).iter().any(Event::is_clear));
}

#[test]
fn test_wire_format_key_order() {
    let show = ShowPage { page: 0, of: 1, lines: s(&["Hi."]), hold_ms: 6000, chirp: true, r#final: true };
    assert_eq!(
        serde_json::to_string(&show.to_wire()).unwrap(),
        r#"{"cmd":"caption","page":0,"of":1,"lines":["Hi."],"hold_ms":6000,"chirp":true,"final":true}"#
    );
    assert_eq!(serde_json::to_string(&Clear.to_wire()).unwrap(), r#"{"cmd":"caption","clear":true}"#);
}
