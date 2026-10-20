//! What earns a place on the robot's own screen.
//!
//! The diary writes down a great deal (diary.py): that is what a memory is for,
//! and a record nobody reads costs nothing. A screen is the opposite. eve sits
//! on a desk all day in someone's peripheral vision, and every line it puts up is
//! a small claim on their attention. Two thoughts that say nearly the same thing
//! are worth keeping and not worth showing twice.
//!
//! So the screen has its own gate, stricter than the memory's and independent of
//! it. The rules, and where the numbers come from:
//!
//! * **A floor on the thought itself.** Importance >= 6, novelty >= 7, or a cool
//!   score >= 0.70. Deliberately *not* "the diary wrote it": the diary's write
//!   gate also fires after three hours of silence, which writes thoughts that are
//!   dull by construction, and frequent low-importance messages are the strongest
//!   predictor of a person muting a source outright (Sahami Shirazi et al., *Large-
//!   scale assessment of mobile notifications*, CHI 2014).
//! * **The subject veto, which does the real work.** A different sentence about
//!   the same vent is still the vent. Tags overlapping a recent caption by
//!   [`SUBJECT_OVERLAP`] inside [`SUBJECT_WINDOW_SECS`] are refused. This is the
//!   load-bearing rule: measured on eve's own repetitive pairs, no surface
//!   similarity measure separates them at any threshold anyone ships — "the vent
//!   is still there" against "the vent is annoyingly centered today" scores 0.25
//!   content-word Jaccard, far under the 0.7-0.85 that de-duplication pipelines
//!   use. Only the subject tells them apart.
//! * **A phrase veto and a similarity backstop**, for literal restatement that the
//!   subject rule would miss because the tags happened to differ.
//! * **Silence between captions.** [`MIN_INTERVAL_SECS`] and [`PER_HOUR`]: the
//!   cost of an interruption accrues over about twenty minutes and does not depend
//!   on how relevant the interruption was (Mark, Gudith & Klocke, *The cost of
//!   interrupted work*, CHI 2008), so the rate is capped and nothing can buy an
//!   extra turn.
//! * **A returning subject gets fewer turns each time.** Beyond
//!   [`TOPIC_LIMIT`] showings inside [`TOPIC_WINDOW_SECS`] a subject is done for
//!   the day, however it is phrased.
//!
//! Ubiquitous tags are stripped before any of this: on a desk robot nearly every
//! frame contains "desk" or "ceiling", and a tag that appears in everything
//! identifies nothing.
//!
//! A refused thought is dropped, not queued. By the time the screen frees up
//! eve is looking somewhere else, and a stale caption is worse than none.
//!
//! Pure: no clock of its own, no I/O. The daemon calls [`ThoughtScreen::offer`] with the
//! monotonic time it already has.

pub mod py;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;

// The floor. Any one of these clears it.
pub const FLOOR_IMPORTANCE: i64 = 6;
pub const FLOOR_NOVELTY: i64 = 7;
pub const FLOOR_COOL: f64 = 0.70;

/// tag Jaccard: the same subject
pub const SUBJECT_OVERLAP: f64 = 0.5;
pub const SUBJECT_WINDOW_SECS: f64 = 20.0 * 60.0;
/// showings of one subject inside...
pub const TOPIC_LIMIT: i64 = 3;
/// ... this window
pub const TOPIC_WINDOW_SECS: f64 = 6.0 * 3600.0;
/// content-word Jaccard: a backstop, not the main rule
pub const SIMILARITY: f64 = 0.34;
/// captions compared against, about two hours of them
pub const HISTORY: i64 = 12;
pub const MIN_INTERVAL_SECS: f64 = 240.0;
pub const PER_HOUR: i64 = 6;
/// a tag in this share of recent captions identifies nothing
pub const UBIQUITY: f64 = 0.5;

static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[a-z][a-z'-]+").expect("word pattern"));

const STOPWORDS_TEXT: &str = "
a an and are as at be been being but by can could did do does doing done for from had has have
having he her hers him his how i if in into is it its just like me might must my no nor not now
of off on once only or other our out over own same she should so some still such than that the
their them then there these they this those through to too under until up very was we were what
when where which while who whom why will with would you your yours
";

/// Words that say nothing about what eve saw. Overlap on these is not
/// similarity, and leaving them in makes every sentence look like every other.
pub static STOPWORDS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| STOPWORDS_TEXT.split_whitespace().collect());

fn content_iter(text: &str) -> Vec<String> {
    let lowered = text.to_lowercase();
    WORD.find_iter(&lowered)
        .map(|m| m.as_str())
        .filter(|w| !STOPWORDS.contains(w) && w.chars().count() > 2)
        .map(str::to_string)
        .collect()
}

/// The words that carry what a sentence is about.
pub fn content_words(text: &str) -> BTreeSet<String> {
    content_iter(text).into_iter().collect()
}

pub fn content_sequence(text: &str) -> Vec<String> {
    content_iter(text)
}

/// Jaccard over content words: 1.0 is the same sentence, 0.0 shares nothing.
pub fn similarity(a: &str, b: &str) -> f64 {
    let (x, y) = (content_words(a), content_words(b));
    if x.is_empty() || y.is_empty() {
        return 0.0;
    }
    x.intersection(&y).count() as f64 / x.union(&y).count() as f64
}

/// Content-word triples — a repeated turn of phrase, whatever surrounds it.
pub fn trigrams(text: &str) -> BTreeSet<(String, String, String)> {
    let seq = content_sequence(text);
    seq.windows(3).map(|w| (w[0].clone(), w[1].clone(), w[2].clone())).collect()
}

pub fn tag_overlap(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    a.intersection(b).count() as f64 / a.union(b).count() as f64
}

/// `{str(t).lower() for t in tags if str(t).strip()}`
fn wanted_tags<I>(tags: I) -> BTreeSet<String>
where
    I: IntoIterator,
    I::Item: AsRef<str>,
{
    tags.into_iter()
        .filter(|t| py::has_content(t.as_ref()))
        .map(|t| t.as_ref().to_lowercase())
        .collect()
}

/// One caption that went up: when, what it said, its tags and the subject they came down to.
#[derive(Debug, Clone, PartialEq)]
pub struct Shown {
    pub at: f64,
    pub text: String,
    pub tags: BTreeSet<String>,
    pub subject: BTreeSet<String>,
}

/// Decides which thoughts reach the screen, and remembers what it showed.
#[derive(Debug, Clone, PartialEq)]
pub struct ThoughtScreen {
    pub floor_importance: i64,
    pub floor_novelty: i64,
    pub floor_cool: f64,
    pub similarity: f64,
    pub history: i64,
    pub subject_overlap: f64,
    pub subject_window_secs: f64,
    pub topic_limit: i64,
    pub topic_window_secs: f64,
    pub min_interval_secs: f64,
    pub per_hour: i64,
    pub ubiquity: f64,
    pub shown: Vec<Shown>,
    pub refused: i64,
}

impl Default for ThoughtScreen {
    fn default() -> Self {
        Self {
            floor_importance: FLOOR_IMPORTANCE,
            floor_novelty: FLOOR_NOVELTY,
            floor_cool: FLOOR_COOL,
            similarity: SIMILARITY,
            history: HISTORY,
            subject_overlap: SUBJECT_OVERLAP,
            subject_window_secs: SUBJECT_WINDOW_SECS,
            topic_limit: TOPIC_LIMIT,
            topic_window_secs: TOPIC_WINDOW_SECS,
            min_interval_secs: MIN_INTERVAL_SECS,
            per_hour: PER_HOUR,
            ubiquity: UBIQUITY,
            shown: Vec::new(),
            refused: 0,
        }
    }
}

impl ThoughtScreen {
    pub fn new() -> Self {
        Self::default()
    }

    /// `self.shown[-self.history:]`
    fn recent(&self) -> &[Shown] {
        &self.shown[py::tail_start(self.shown.len(), self.history)..]
    }

    // -- what a thought is about -------------------------------------------------

    /// Tags so common in what eve has shown that they name nothing.
    pub fn ubiquitous_tags(&self) -> BTreeSet<String> {
        let recent = self.recent();
        if recent.len() < 4 {
            return BTreeSet::new();
        }
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for s in recent {
            for tag in &s.tags {
                *counts.entry(tag.as_str()).or_insert(0) += 1;
            }
        }
        let limit = self.ubiquity * recent.len() as f64;
        counts.into_iter().filter(|&(_, n)| n as f64 >= limit).map(|(t, _)| t.to_string()).collect()
    }

    /// The tags that actually identify this thought.
    pub fn subject_of<I>(&self, tags: I) -> BTreeSet<String>
    where
        I: IntoIterator,
        I::Item: AsRef<str>,
    {
        let wanted = wanted_tags(tags);
        let ubiquitous = self.ubiquitous_tags();
        let narrowed: BTreeSet<String> = wanted.difference(&ubiquitous).cloned().collect();
        if narrowed.is_empty() { wanted } else { narrowed }
    }

    pub fn clears_the_floor(&self, importance: i64, novelty: i64, cool: f64) -> bool {
        importance >= self.floor_importance || novelty >= self.floor_novelty || cool >= self.floor_cool
    }

    // -- the gate ----------------------------------------------------------------

    /// Why this thought would not go up, or None if it would. Pure.
    pub fn refusal<I>(
        &self,
        now: f64,
        thought: &str,
        tags: I,
        importance: i64,
        novelty: i64,
        cool: f64,
    ) -> Option<String>
    where
        I: IntoIterator,
        I::Item: AsRef<str>,
    {
        let text = py::squash(thought);
        if text.is_empty() {
            return Some("empty".to_string());
        }
        if !self.clears_the_floor(importance, novelty, cool) {
            return Some(format!(
                "below the floor (importance {importance}, novelty {novelty}, cool {})",
                py::fixed(cool, 2)
            ));
        }
        if let Some(last) = self.shown.last() {
            let since = now - last.at;
            if since < self.min_interval_secs {
                return Some(format!("only {}s since the last one", py::fixed(since, 0)));
            }
        }
        let this_hour = self.shown.iter().filter(|s| now - s.at < 3600.0).count() as i64;
        if this_hour >= self.per_hour {
            return Some(format!("{} already this hour", self.per_hour));
        }

        let recent = self.recent();
        let subject = self.subject_of(tags);
        for s in recent {
            if now - s.at <= self.subject_window_secs
                && tag_overlap(&subject, &s.subject) >= self.subject_overlap
            {
                let shared: Vec<&str> = subject.intersection(&s.subject).map(String::as_str).collect();
                return Some(format!("about {} again", shared.join("/")));
            }
        }
        if !subject.is_empty() {
            let seen = self
                .shown
                .iter()
                .filter(|s| {
                    now - s.at <= self.topic_window_secs
                        && tag_overlap(&subject, &s.subject) >= self.subject_overlap
                })
                .count() as i64;
            if seen >= self.topic_limit {
                let all: Vec<&str> = subject.iter().map(String::as_str).collect();
                return Some(format!("{seen} about {} today already", all.join("/")));
            }
        }
        let tri = trigrams(&text);
        for s in recent {
            if !tri.is_empty() {
                let theirs = trigrams(&s.text);
                // BTreeSet iterates in sorted order, so the first shared triple is `sorted(...)[0]`.
                if let Some(first) = tri.intersection(&theirs).next() {
                    return Some(format!("phrase already used: {}", py::repr_triple(first)));
                }
            }
            let score = similarity(&text, &s.text);
            if score >= self.similarity {
                let head: String = s.text.chars().take(40).collect();
                return Some(format!("says the same as {} ({})", py::repr_str(&head), py::fixed(score, 2)));
            }
        }
        None
    }

    /// Show this thought? Records it when the answer is yes.
    pub fn offer<I>(&mut self, now: f64, thought: &str, tags: I, importance: i64, novelty: i64, cool: f64) -> bool
    where
        I: IntoIterator,
        I::Item: AsRef<str>,
    {
        // read twice, by refusal and by record: an iterator would be spent
        let tags: Vec<String> = tags.into_iter().map(|t| t.as_ref().to_string()).collect();
        if let Some(why) = self.refusal(now, thought, &tags, importance, novelty, cool) {
            self.refused += 1;
            tracing::debug!("screen: not showing ({})", why);
            return false;
        }
        self.record(now, thought, &tags);
        true
    }

    /// Note a thought as shown, without judging it: for a caller that already asked refusal() (the daemon
    /// does, to log the reason), so the screen is not run twice.
    pub fn record<I>(&mut self, now: f64, thought: &str, tags: I)
    where
        I: IntoIterator,
        I::Item: AsRef<str>,
    {
        let text = py::squash(thought);
        let wanted = wanted_tags(tags);
        let subject = self.subject_of(&wanted);
        self.shown.push(Shown { at: now, text, tags: wanted, subject });
        // Keep enough history for the longest window that counts back over it.
        let keep = self.history.max(self.per_hour.saturating_mul(8));
        if (self.shown.len() as i64) > keep {
            // del self.shown[:-keep]
            let end = py::slice_index(self.shown.len(), keep.saturating_neg());
            self.shown.drain(..end);
        }
    }
}

#[cfg(test)]
mod tests;
