//! What a browser request asks for, as one outcome: search, open a site, open a result, or none of these.
//!
//! Port of `eve_bridge/browser_intent.py`. Two halves, split the way LayaBrowse splits them
//! (github.com/cklxx/LayaBrowse, goal_engine.py):
//!
//! * Code reads the literal arguments and nothing else: the site (from the table below, after the site names
//!   speech recognition splits are joined), the query (the rest of the clause after a search phrase: "search for",
//!   "look up", "search the web for", "web search", "search <site> for", ...) and a result number ("the first
//!   result", "open number two", "play the sixth"). Code never guesses a kind.
//! * Laya picks only the KIND, from that finite set, using the browser-trained checkpoint cklxx/laya-browser (v10s,
//!   pinned revision) through laya-mlx, on this Mac. It is asked the way LayaBrowse asks its outcome question: one
//!   Choice whose goal is the request, whose rules say "the outcome, not the next action", and whose options are
//!   the fixed outcome descriptions plus "unsupported". Its answer passes the same schema check and the same
//!   0.50 / 0.10 gate (top probability, margin over the runner-up) before anything uses it. The goal it reads is
//!   the request with disfluencies dropped and, in a one-step request, navigation verbs said as "open"
//!   ([`laya_goal`]).
//!
//! [`expected_outcome`] returns `None` — an abstention — when the switch is off, laya-mlx is missing, the
//! checkpoint is not cached or fails to load, the answer is malformed or under the gate, or Laya's kind cannot be
//! carried by the literal arguments (a search with no query, a site that was never named, a result with no
//! number). A "none" outcome is an answer: the request is not one of these three.
//!
//! EVE_BROWSER_INTENT=laya|off (default laya). EVE_BROWSER_INTENT_IDLE_S (default 600): Laya is unloaded after
//! that many seconds with no question and loaded again on the next (0 keeps it loaded). Nothing leaves the Mac;
//! Jev is not asked.
//!
//! laya-mlx is a Python-only MLX runtime, so the loaded model lives in a helper Python process
//! (`src/laya_helper.py`, see [`LayaSubprocess`]); everything else is native.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{mpsc, Arc, LazyLock, OnceLock, Weak};
use std::time::{Duration, Instant};

use fancy_regex::Regex;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

// ---- Python string helpers ---------------------------------------------------------------------------------

/// Python's `str.isspace` for one character: Unicode White_Space plus the C0 separators U+001C..U+001F.
fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `" ".join(text.split())`.
fn squash(text: &str) -> String {
    text.split(is_py_space).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

/// `text.strip()`.
fn py_strip(text: &str) -> &str {
    text.trim_matches(is_py_space)
}

/// `text.strip(chars)`.
fn strip_chars<'a>(text: &'a str, chars: &str) -> &'a str {
    text.trim_matches(|c| chars.contains(c))
}

/// `str.casefold()`: lowercasing plus the full case foldings that differ from it in Latin and Greek text.
fn casefold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            'ß' | 'ẞ' => out.push_str("ss"),
            'ſ' => out.push('s'),
            'ς' => out.push('σ'),
            'ﬀ' => out.push_str("ff"),
            'ﬁ' => out.push_str("fi"),
            'ﬂ' => out.push_str("fl"),
            'ﬃ' => out.push_str("ffi"),
            'ﬄ' => out.push_str("ffl"),
            'ﬅ' | 'ﬆ' => out.push_str("st"),
            _ => out.extend(c.to_lowercase()),
        }
    }
    out
}

/// A pattern with no backtracking limit: Python's `re` has none, so a long request that needs many steps still
/// matches as it does there (fancy-regex's default limit of one million steps silently lost the site in a
/// request a few thousand words long).
fn re(pattern: &str) -> Regex {
    fancy_regex::RegexBuilder::new(pattern)
        .backtrack_limit(usize::MAX)
        .build()
        .unwrap_or_else(|e| panic!("browser_intent pattern {pattern:?}: {e}"))
}

/// The first match. With no backtracking limit the matcher cannot fail; an error would read as no match.
fn find<'t>(regex: &Regex, text: &'t str) -> Option<fancy_regex::Match<'t>> {
    regex.find(text).ok().flatten()
}

fn captures<'t>(regex: &Regex, text: &'t str) -> Option<fancy_regex::Captures<'t, str>> {
    regex.captures(text).ok().flatten()
}

/// `regex.sub(repl, text)`, with `repl` called on each match (the patterns here never match empty).
fn sub(regex: &Regex, text: &str, mut repl: impl FnMut(&fancy_regex::Captures<str>) -> String) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for caps in regex.captures_iter(text) {
        let Ok(caps) = caps else { break };
        let m = caps.get(0).expect("group 0");
        out.push_str(&text[last..m.start()]);
        out.push_str(&repl(&caps));
        last = m.end();
    }
    out.push_str(&text[last..]);
    out
}

fn sub_empty(regex: &Regex, text: &str) -> String {
    sub(regex, text, |_| String::new())
}

static SPLIT_SEP: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"[\s-]+").expect("separator"));

/// `re.split(r"[\s-]+", text)`.
fn split_spoken(text: &str) -> Vec<&str> {
    SPLIT_SEP.split(text).collect()
}

// ---- the site table ----------------------------------------------------------------------------------------
//
// Hosts, homes, search URLs and query keys for GitHub, YouTube, Wikipedia, eBay and Google are LayaBrowse's
// (sites/*.json, MIT, Copyright (c) 2026 Aryan Bhujade); the rest are each site's own public search address.
// A site with no search address the URL can carry has only a home: it can be opened, never searched by code.

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Site {
    /// the site's own spelling: "GitHub", "Stack Overflow"
    pub name: &'static str,
    pub host: &'static str,
    pub home: &'static str,
    /// "{query}" is replaced by the URL-encoded query
    pub search_template: &'static str,
    /// the query parameter in search_template; "" when the query is in the path
    pub query_key: &'static str,
    /// how speech recognition splits the name ("git hub")
    pub split: &'static [&'static str],
    /// split forms that are also ordinary English: joined only after a cue word
    pub ambiguous: &'static [&'static str],
    /// other names people say for it ("nyt", "x.com")
    pub aliases: &'static [&'static str],
    /// the name is also an ordinary word ("target", "medium"): a site only after a cue
    pub everyday: bool,
}

/// `urllib.parse.quote(text, safe="")`, with `plus` as `quote_plus`: UTF-8 bytes, `A-Za-z0-9_.-~` kept.
fn percent_encode(text: &str, plus: bool) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for &b in text.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => out.push(b as char),
            b' ' if plus => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

impl Site {
    const BASE: Site = Site {
        name: "",
        host: "",
        home: "",
        search_template: "",
        query_key: "",
        split: &[],
        ambiguous: &[],
        aliases: &[],
        everyday: false,
    };

    pub fn search_url(&self, query: &str) -> String {
        if self.search_template.is_empty() {
            return String::new();
        }
        // In a query string a space is "+"; in a path it must be "%20", and nothing may stay unescaped.
        let before = self.search_template.split("{query}").next().unwrap_or("");
        let in_path = !before.contains('?');
        let encoded = percent_encode(query, !in_path);
        self.search_template.replace("{query}", &encoded)
    }
}

const fn site(name: &'static str, host: &'static str, home: &'static str) -> Site {
    Site { name, host, home, ..Site::BASE }
}

const fn searched(
    name: &'static str,
    host: &'static str,
    home: &'static str,
    search_template: &'static str,
    query_key: &'static str,
) -> Site {
    Site { name, host, home, search_template, query_key, ..Site::BASE }
}

pub static SITES: &[Site] = &[
    Site {
        split: &["git hub"],
        ..searched("GitHub", "github.com", "https://github.com/",
                   "https://github.com/search?q={query}&type=repositories", "q")
    },
    Site {
        split: &["you tube"],
        ..searched("YouTube", "youtube.com", "https://www.youtube.com/",
                   "https://www.youtube.com/results?search_query={query}", "search_query")
    },
    Site {
        split: &["wiki pedia", "wikki pedia"],
        ..searched("Wikipedia", "wikipedia.org", "https://en.wikipedia.org/wiki/Main_Page",
                   "https://en.wikipedia.org/w/index.php?search={query}", "search")
    },
    Site {
        split: &["e bay", "e-bay"],
        ..searched("eBay", "ebay.com", "https://www.ebay.com/", "https://www.ebay.com/sch/i.html?_nkw={query}", "_nkw")
    },
    searched("Google", "google.com", "https://www.google.com/", "https://www.google.com/search?q={query}", "q"),
    Site {
        split: &["duck duck go", "duckduck go", "duck duckgo"],
        ..searched("DuckDuckGo", "duckduckgo.com", "https://duckduckgo.com/", "https://duckduckgo.com/?q={query}", "q")
    },
    searched("Bing", "bing.com", "https://www.bing.com/", "https://www.bing.com/search?q={query}", "q"),
    searched("Yahoo", "yahoo.com", "https://www.yahoo.com/", "https://search.yahoo.com/search?p={query}", "p"),
    Site {
        split: &["stack overflow"],
        ambiguous: &["stack overflow"],
        ..searched("Stack Overflow", "stackoverflow.com", "https://stackoverflow.com/",
                   "https://stackoverflow.com/search?q={query}", "q")
    },
    Site {
        split: &["red dit"],
        ..searched("Reddit", "reddit.com", "https://www.reddit.com/", "https://www.reddit.com/search/?q={query}", "q")
    },
    Site {
        split: &["linked in"],
        ambiguous: &["linked in"],
        ..searched("LinkedIn", "linkedin.com", "https://www.linkedin.com/",
                   "https://www.linkedin.com/search/results/all/?keywords={query}", "keywords")
    },
    Site {
        split: &["net flix"],
        ..searched("Netflix", "netflix.com", "https://www.netflix.com/", "https://www.netflix.com/search?q={query}", "q")
    },
    searched("Amazon", "amazon.com", "https://www.amazon.com/", "https://www.amazon.com/s?k={query}", "k"),
    // Two words in its own spelling: nothing to join, but "hacker news" still names the site. hn.algolia.com
    // rewrites ?q= to ?query= on load; plan_contract.REWRITTEN_KEYS accepts both.
    Site {
        split: &["hacker news"],
        ambiguous: &["hacker news"],
        ..searched("Hacker News", "news.ycombinator.com", "https://news.ycombinator.com/",
                   "https://hn.algolia.com/?q={query}", "q")
    },
    // Developer and reference
    Site {
        split: &["n p m"],
        ..searched("npm", "npmjs.com", "https://www.npmjs.com/", "https://www.npmjs.com/search?q={query}", "q")
    },
    Site {
        split: &["py pi"],
        ..searched("PyPI", "pypi.org", "https://pypi.org/", "https://pypi.org/search/?q={query}", "q")
    },
    Site {
        split: &["hugging face"],
        ambiguous: &["hugging face"],
        ..searched("Hugging Face", "huggingface.co", "https://huggingface.co/",
                   "https://huggingface.co/models?search={query}", "search")
    },
    searched("arXiv", "arxiv.org", "https://arxiv.org/", "https://arxiv.org/search/?query={query}&searchtype=all",
             "query"),
    searched("Wolfram Alpha", "wolframalpha.com", "https://www.wolframalpha.com/",
             "https://www.wolframalpha.com/input?i={query}", "i"),
    searched("Quora", "quora.com", "https://www.quora.com/", "https://www.quora.com/search?q={query}", "q"),
    Site {
        everyday: true,
        ..searched("Medium", "medium.com", "https://medium.com/", "https://medium.com/search?q={query}", "q")
    },
    // News and sport
    Site {
        split: &["b b c"],
        ..searched("BBC", "bbc.co.uk", "https://www.bbc.co.uk/", "https://www.bbc.co.uk/search?q={query}", "q")
    },
    Site {
        split: &["c n n"],
        ..searched("CNN", "cnn.com", "https://edition.cnn.com/", "https://edition.cnn.com/search?q={query}", "q")
    },
    Site {
        aliases: &["nyt", "the new york times"],
        ..searched("New York Times", "nytimes.com", "https://www.nytimes.com/",
                   "https://www.nytimes.com/search?query={query}", "query")
    },
    Site {
        split: &["e s p n"],
        ..searched("ESPN", "espn.com", "https://www.espn.com/", "https://www.espn.com/search/_/q/{query}", "")
    },
    // Video, music, social
    Site {
        split: &["i m d b"],
        ..searched("IMDb", "imdb.com", "https://www.imdb.com/", "https://www.imdb.com/find/?q={query}", "q")
    },
    searched("Spotify", "spotify.com", "https://open.spotify.com/", "https://open.spotify.com/search/{query}", ""),
    Site {
        split: &["sound cloud"],
        ..searched("SoundCloud", "soundcloud.com", "https://soundcloud.com/", "https://soundcloud.com/search?q={query}",
                   "q")
    },
    searched("Vimeo", "vimeo.com", "https://vimeo.com/", "https://vimeo.com/search?q={query}", "q"),
    Site {
        everyday: true,
        ..searched("Twitch", "twitch.tv", "https://www.twitch.tv/", "https://www.twitch.tv/search?term={query}", "term")
    },
    Site {
        aliases: &["x.com"],
        ..searched("Twitter", "x.com", "https://x.com/", "https://x.com/search?q={query}", "q")
    },
    Site {
        split: &["face book"],
        ..searched("Facebook", "facebook.com", "https://www.facebook.com/",
                   "https://www.facebook.com/search/top/?q={query}", "q")
    },
    Site { split: &["insta gram"], ..site("Instagram", "instagram.com", "https://www.instagram.com/") },
    Site {
        split: &["tik tok"],
        ..searched("TikTok", "tiktok.com", "https://www.tiktok.com/", "https://www.tiktok.com/search?q={query}", "q")
    },
    searched("Pinterest", "pinterest.com", "https://www.pinterest.com/",
             "https://www.pinterest.com/search/pins/?q={query}", "q"),
    Site {
        split: &["good reads"],
        ambiguous: &["good reads"],
        ..searched("Goodreads", "goodreads.com", "https://www.goodreads.com/",
                   "https://www.goodreads.com/search?q={query}", "q")
    },
    Site {
        everyday: true,
        ..searched("Steam", "steampowered.com", "https://store.steampowered.com/",
                   "https://store.steampowered.com/search/?term={query}", "term")
    },
    // Shopping
    searched("Etsy", "etsy.com", "https://www.etsy.com/", "https://www.etsy.com/search?q={query}", "q"),
    searched("Walmart", "walmart.com", "https://www.walmart.com/", "https://www.walmart.com/search?q={query}", "q"),
    Site {
        everyday: true,
        ..searched("Target", "target.com", "https://www.target.com/", "https://www.target.com/s?searchTerm={query}",
                   "searchTerm")
    },
    Site {
        split: &["best buy"],
        ambiguous: &["best buy"],
        ..searched("Best Buy", "bestbuy.com", "https://www.bestbuy.com/",
                   "https://www.bestbuy.com/site/searchpage.jsp?st={query}", "st")
    },
    searched("Costco", "costco.com", "https://www.costco.com/", "https://www.costco.com/CatalogSearch?keyword={query}",
             "keyword"),
    searched("IKEA", "ikea.com", "https://www.ikea.com/", "https://www.ikea.com/us/en/search/?q={query}", "q"),
    Site {
        aliases: &["the home depot"],
        ..searched("Home Depot", "homedepot.com", "https://www.homedepot.com/", "https://www.homedepot.com/s/{query}",
                   "")
    },
    Site {
        everyday: true,
        ..searched("Apple", "apple.com", "https://www.apple.com/", "https://www.apple.com/us/search/{query}", "")
    },
    Site {
        split: &["craigs list"],
        ..searched("Craigslist", "craigslist.org", "https://www.craigslist.org/",
                   "https://www.craigslist.org/search/sss?query={query}", "query")
    },
    // Places, travel, jobs
    searched("Google Maps", "maps.google.com", "https://www.google.com/maps", "https://www.google.com/maps/search/{query}",
             ""),
    Site { split: &["g mail"], ..site("Gmail", "mail.google.com", "https://mail.google.com/") },
    searched("Yelp", "yelp.com", "https://www.yelp.com/", "https://www.yelp.com/search?find_desc={query}", "find_desc"),
    searched("Zillow", "zillow.com", "https://www.zillow.com/", "https://www.zillow.com/homes/{query}_rb/", ""),
    Site {
        split: &["trip advisor"],
        ambiguous: &["trip advisor"],
        ..searched("Tripadvisor", "tripadvisor.com", "https://www.tripadvisor.com/",
                   "https://www.tripadvisor.com/Search?q={query}", "q")
    },
    Site {
        split: &["air bnb", "air b n b", "air b and b"],
        ..searched("Airbnb", "airbnb.com", "https://www.airbnb.com/", "https://www.airbnb.com/s/{query}/homes", "")
    },
    Site {
        aliases: &["booking"],
        everyday: true,
        ..searched("Booking.com", "booking.com", "https://www.booking.com/",
                   "https://www.booking.com/searchresults.html?ss={query}", "ss")
    },
    site("Expedia", "expedia.com", "https://www.expedia.com/"),
    Site {
        everyday: true,
        ..searched("Indeed", "indeed.com", "https://www.indeed.com/", "https://www.indeed.com/jobs?q={query}", "q")
    },
    Site {
        split: &["glass door"],
        ambiguous: &["glass door"],
        ..searched("Glassdoor", "glassdoor.com", "https://www.glassdoor.com/",
                   "https://www.glassdoor.com/Search/results.htm?keyword={query}", "keyword")
    },
];

/// The word just before an ambiguous form that makes it a site: "open linked in", "on stack overflow". Without one
/// ("the doc I linked in slack", "a stack overflow error") the words are left as they are.
pub const SITE_CUES: &[&str] = &[
    "open", "visit", "on", "to", "at", "launch", "check", "browse", "search", "onto", "via", "using", "into",
];
/// An everyday word ("target", "medium", "steam") names a site only after one of these, or written as a domain:
/// "on medium heat" and "let off some steam" are not sites. "to" counts only after a verb of going ("go to target").
pub const EVERYDAY_CUES: &[&str] = &["open", "visit", "launch", "browse", "search", "onto", "via", "using", "at", "from"];
const GOING: &[&str] = &["go", "head", "me", "navigate", "get", "back", "switch"];
const DOMAIN: &str = r"(?:\.(?:com|org|net|co\.uk|co|tv|io))?";

/// `_spoken`: a form as a pattern whose words may be split by spaces or hyphens.
fn spoken(form: &str) -> String {
    split_spoken(form).iter().map(|part| regex::escape(part)).collect::<Vec<_>>().join(r"[\s-]+")
}

/// The host's own name ("imdb" for imdb.com, "bbc" for bbc.co.uk) when that is the whole host, so a subdomain host
/// (mail.google.com, news.ycombinator.com) or a one-letter one (x.com) names nothing by itself.
fn host_label(site: &Site) -> Option<&'static str> {
    let mut labels: Vec<&'static str> = site.host.split('.').collect();
    if labels.len() >= 2 && labels[labels.len() - 2..] == ["co", "uk"] {
        labels.pop();
    }
    (labels.len() == 2 && labels[0].chars().count() >= 3).then_some(labels[0])
}

struct Table {
    split_re: Regex,
    by_form: HashMap<String, &'static Site>,
    ambiguous: HashSet<String>,
    names: HashMap<String, &'static Site>,
    name_alt: String,
    name_re: Regex,
}

static TABLE: LazyLock<Table> = LazyLock::new(|| {
    let mut splits: Vec<(&'static str, &'static Site)> =
        SITES.iter().flat_map(|site| site.split.iter().map(move |form| (*form, site))).collect();
    splits.sort_by_key(|(form, _)| std::cmp::Reverse(form.chars().count()));
    let split_alt = splits.iter().map(|(f, _)| spoken(f)).collect::<Vec<_>>().join("|");
    let split_re = re(&format!(r"(?i)(?<![\w.])({split_alt})(?![\w])"));
    let mut by_form = HashMap::new();
    for (form, site) in &splits {
        by_form.insert(split_spoken(&casefold(form)).join(" "), *site);
    }
    let ambiguous = SITES.iter().flat_map(|s| s.ambiguous.iter().map(|f| squash(f))).collect();
    let mut names: HashMap<String, &'static Site> = HashMap::new();
    for site in SITES {
        let joined = site.name.replace(' ', "");
        let mut keys: Vec<String> = vec![site.name.to_string(), joined];
        keys.extend(host_label(site).map(str::to_string));
        keys.extend(site.aliases.iter().map(|a| a.to_string()));
        for key in keys {
            if !key.is_empty() {
                names.entry(casefold(&key)).or_insert(site);
            }
        }
    }
    let mut forms: Vec<String> = SITES
        .iter()
        .flat_map(|site| {
            let mut v = vec![site.name.to_string(), site.name.replace(' ', ""), host_label(site).unwrap_or("").to_string()];
            v.extend(site.aliases.iter().map(|a| a.to_string()));
            v
        })
        .filter(|f| !f.is_empty())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    forms.sort_by_key(|f| std::cmp::Reverse(f.chars().count()));
    let name_alt = forms
        .iter()
        .map(|f| f.split(' ').map(regex::escape).collect::<Vec<_>>().join(r"[\s-]+"))
        .collect::<Vec<_>>()
        .join("|");
    let name_re = re(&format!(r"(?i)(?<![\w.])({name_alt})({DOMAIN})(?![\w])"));
    Table { split_re, by_form, ambiguous, names, name_alt, name_re }
});

static CUE_WORD: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"[a-z']+").expect("cue word"));

/// `_cued`: is the form at byte `start` of `text` preceded by a cue word?
fn cued(text: &str, start: usize, everyday: bool) -> bool {
    let folded = casefold(&text[..start]);
    let before: Vec<&str> = CUE_WORD.find_iter(&folded).map(|m| m.as_str()).collect();
    if !everyday {
        return before.last().is_none_or(|w| SITE_CUES.contains(w));
    }
    let Some(last) = before.last() else { return false };
    EVERYDAY_CUES.contains(last) || (*last == "to" && before.len() > 1 && GOING.contains(&before[before.len() - 2]))
}

/// The text with split site names joined in the site's own spelling ("git hub" → "GitHub"); nothing else
/// changes. A split form that is also ordinary English is joined only after a cue word ("open linked in").
pub fn join_site_names(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let table = &*TABLE;
    sub(&table.split_re, text, |caps| {
        let m = caps.get(0).expect("group 0");
        let form = split_spoken(&casefold(m.as_str())).join(" ");
        if table.ambiguous.contains(&form) && !cued(text, m.start(), false) {
            return m.as_str().to_string();
        }
        match table.by_form.get(&form) {
            Some(site) => site.name.to_string(),
            None => m.as_str().to_string(),
        }
    })
}

/// The site a name, a joined name, an alias or a split form names; None for anything else.
pub fn site_for(name: &str) -> Option<&'static Site> {
    let table = &*TABLE;
    let key = split_spoken(&casefold(py_strip(name))).join(" ");
    table
        .names
        .get(&key)
        .or_else(|| table.names.get(&key.replace(' ', "")))
        .or_else(|| table.by_form.get(&key))
        .copied()
}

/// The first site the request names, after joining. An ambiguous two-word name ("stack overflow") counts only in
/// the site's own capitals or after a cue word, so "a stack overflow error" names no site; an everyday word
/// ("target") counts only after a cue word or written as a domain ("medium.com").
pub fn mentioned_site(text: &str) -> Option<&'static Site> {
    let table = &*TABLE;
    let joined = join_site_names(text);
    for caps in table.name_re.captures_iter(&joined) {
        let Ok(caps) = caps else { break };
        let said = caps.get(1).map_or("", |g| g.as_str());
        let Some(site) = site_for(said) else { continue };
        let start = caps.get(0).expect("group 0").start();
        if table.ambiguous.contains(&casefold(site.name)) && said.contains(' ') && said != site.name
            && !cued(&joined, start, false)
        {
            continue;
        }
        let domain = caps.get(2).map_or("", |g| g.as_str());
        if site.everyday && domain.is_empty() && !said.contains('.') && !cued(&joined, start, true) {
            continue;
        }
        return Some(site);
    }
    None
}

// ---- literal arguments -------------------------------------------------------------------------------------

/// Ordinal words and their result numbers; "last" has none.
pub const ORDINALS: &[(&str, Option<u32>)] = &[
    ("first", Some(1)), ("top", Some(1)), ("second", Some(2)), ("third", Some(3)), ("fourth", Some(4)),
    ("fifth", Some(5)), ("sixth", Some(6)), ("seventh", Some(7)), ("eighth", Some(8)), ("ninth", Some(9)),
    ("tenth", Some(10)), ("last", None),
];
pub const NUMBERS: &[(&str, u32)] = &[
    ("one", 1), ("two", 2), ("three", 3), ("four", 4), ("five", 5), ("six", 6), ("seven", 7), ("eight", 8),
    ("nine", 9), ("ten", 10),
];
pub const RESULT_NOUN: &str =
    r"(?:result|video|link|item|one|repo|repository|article|listing|product|hit|story|post|page|song|track)s?";

static ORD_ALT: LazyLock<String> = LazyLock::new(|| {
    let words: Vec<&str> = ORDINALS.iter().map(|(k, _)| *k).filter(|k| *k != "last").collect();
    words.join("|") + r"|\d{1,2}(?:st|nd|rd|th)"
});
static NUM_ALT: LazyLock<String> =
    LazyLock::new(|| r"\d{1,2}|".to_string() + &NUMBERS.iter().map(|(k, _)| *k).collect::<Vec<_>>().join("|"));
static ORDINAL_RE: LazyLock<Regex> =
    LazyLock::new(|| re(&format!(r"(?i)\b({})\s+(?:[a-z]+\s+)?{RESULT_NOUN}\b", *ORD_ALT)));
static NUMBER_RE: LazyLock<Regex> =
    LazyLock::new(|| re(&format!(r"(?i)\b(?:result|video|link|item)\s+(?:number\s+)?({})\b", *NUM_ALT)));
// A result number with no noun is read only where nothing else could follow it: after a verb of picking ("open
// number two", "play the sixth") and at the end of the clause, or as the whole request ("the seventh"). An ordinal
// with words after it ("the first moon landing", "the sixth president") is not a result number.
const PICK_VERB: &str = r"(?:open|click|play|pick|select|choose|tap|watch|take|show\s+me|read|try|hit|press|listen\s+to|go\s+(?:with|for|to|into|in\s+to)|bring\s+up|pull\s+up)";
const CLAUSE_END: &str =
    r"(?=\s*$|\s*[.,!?;]|\s+(?:please|and|then|one|of\s+(?:them|those|these|the\s+results|the\s+list))\b)";
static BARE_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"(?i)\b{PICK_VERB}\s+(?:on\s+|into\s+)?(?:the\s+)?(?:({})|(?:number|#)\s*({}))\b{CLAUSE_END}",
        *ORD_ALT, *NUM_ALT
    ))
});
static ALONE_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"(?i)^\W*(?:(?:um+|uh+|erm|okay|ok|and)\s+)*(?:(?:the\s+)?({})(?:\s+one)?|(?:number|#)\s*({}))\W*$",
        *ORD_ALT, *NUM_ALT
    ))
});

static DISFLUENCY: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\b(?:u+m+|u+h+|erm|uhm)\b[,]?\s*"));
/// Where a clause starts: a bare "search X" or "google X" is a query only here, so "the search bar" is not one.
const START: &str = r"(?:^|[,.;!?]\s*|\b(?:please|and|then|you|to|just|now|so|ok|okay|also)\s+)";

/// The phrase families that introduce a query, most specific first. Each captures the rest of the clause.
static QUERY_INTROS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    let site_alt = &TABLE.name_alt;
    vec![
        // "search youtube for X", "search on reddit for X", "search youtube X"
        re(&format!(r"(?i)\bsearch\s+(?:on\s+|in\s+)?(?:the\s+)?(?:{site_alt}){DOMAIN}\s+(?:for\s+|about\s+)?(.+)")),
        // "browse airbnb for X", "check ebay for X": a site's own search, said with another verb
        re(&format!(
            r"(?i)\b(?:browse|check|look\s+on|shop\s+on|scan)\s+(?:the\s+)?(?:{site_alt}){DOMAIN}\s+for\s+(.+)"
        )),
        // "find X on ikea", "look for X on etsy", "shop for X at target": only with a site named, never a bare "find"
        re(&format!(
            r"(?i)\b(?:find|look\s+for|shop\s+for|browse\s+for|hunt\s+for)\s+(.+?\s+(?:on|at|in)\s+(?:the\s+)?(?:{site_alt}){DOMAIN}(?![\w]).*)"
        )),
        // "search the web for X", "search online for X", "look on the internet for X", "look online for X"
        re(r"(?i)\b(?:search|look|hunt)\s+(?:(?:on\s+)?the\s+(?:web|internet|net)|online)\s+(?:for\s+|about\s+)?(.+)"),
        // "web search X", "do a (quick) web search for X", "run a google search for X", "do a search on X"
        re(r"(?i)\b(?:(?:do|run|make)\s+(?:a\s+)?)?(?:quick\s+)?(?:web|internet|online|google)?\s*search\s+(?:for|on|about)\s+(.+)"),
        re(r"(?i)\b(?:web|internet|online)\s+search\s+(?:for\s+)?(.+)"),
        re(r"(?i)\b(?:search\s+(?:for|up|about)|look\s+up)\s+(.+)"),
        // "google X" as a verb, at the start of a clause; "google maps" and "google it" are not queries
        re(&format!(r"(?i){START}google\s+(?!(?:maps|docs|drive|sheets|it|that|this)\b)(?:for\s+)?(.+)")),
        // a bare "search X" at the start of a clause, but never "search results", "search bar" and the like
        re(&format!(
            r"(?i){START}search\s+(?!(?:for|up|about|results?|bar|box|field|page|button|engine|history|settings|it|that|this)\b)(.+)"
        )),
    ]
});
static QUERY_END: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    let site_alt = &TABLE.name_alt;
    vec![
        re(&format!(r"(?i)\s+(?:on|in|using|at|from|via)\s+(?:the\s+)?(?:{site_alt}){DOMAIN}\b.*$")),
        re(r"(?i)\s*,?\s*\b(?:and|then)\b\s+(?:then\s+)?(?:open|click|play|show|go|pick|select|watch|choose|read|tap|tell|let|give|summari[sz]e|see|find|check|send|email|text|share|save|download|add|list|compare|explain|log|sign|subscribe|follow|like|comment|reply|post|buy|order|book|turn|copy)\b.*$"),
        re(r"(?i)\s+in\s+(?:a\s+)?new\s+tab\b.*$"),
        re(r"(?i)\s+so\s+(?:that\s+)?(?:i|we|you)\b.*$"),
        re(r"(?i)\s+because\b.*$"),
    ]
});
/// Politeness and filler at either end of a query, removed until nothing more comes off.
static QUERY_TAIL: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)\s+(?:please|for\s+me|real\s+quick|quickly|right\s+now|now|thanks|thank\s+you)$"));
static QUERY_HEAD: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^(?:please|the|a|an|some)\s+"));

/// The literal arguments of a request. Each is None when the request does not say it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arguments {
    pub site: Option<&'static Site>,
    pub query: Option<String>,
    pub n: Option<u32>,
}

const QUERY_PUNCT: &str = " .,!?;:";
const QUERY_PUNCT_QUOTES: &str = " .,!?;:\"'";

/// `_query`: the query a search phrase introduces, filler trimmed; None when there is none.
fn query(text: &str) -> Option<String> {
    let text = sub_empty(&DISFLUENCY, text);
    let caps = QUERY_INTROS.iter().find_map(|intro| captures(intro, &text))?;
    let mut query = caps.get(1).map_or("", |g| g.as_str()).to_string();
    for end in QUERY_END.iter() {
        query = sub_empty(end, &query);
    }
    let mut query: String = strip_chars(py_strip(&query), QUERY_PUNCT).to_string();
    let chars: Vec<char> = query.chars().collect();
    if chars.len() >= 2 && chars[0] == chars[chars.len() - 1] && (chars[0] == '"' || chars[0] == '\'') {
        query = chars[1..chars.len() - 1].iter().collect();
    }
    let mut query = strip_chars(&squash(&query), QUERY_PUNCT_QUOTES).to_string();
    loop {
        let trimmed = strip_chars(&sub_empty(&QUERY_HEAD, &sub_empty(&QUERY_TAIL, &query)), QUERY_PUNCT_QUOTES)
            .to_string();
        if trimmed == query {
            break;
        }
        query = trimmed;
    }
    if query.is_empty() || site_for(&query).is_some() {
        // "search youtube": the only words after the verb name the site, so there is nothing to search for.
        return None;
    }
    Some(query)
}

static IS_DIGIT: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"^\d$").expect("digit"));

/// The value of a Unicode decimal digit (`\d`): every Nd digit sits in a contiguous run of ten that starts at its
/// zero, so the value is the number of digits just before it, mod 10.
fn digit_value(c: char) -> Option<u32> {
    if let Some(v) = c.to_digit(10) {
        return Some(v);
    }
    let is_digit = |ch: char| IS_DIGIT.is_match(ch.encode_utf8(&mut [0u8; 4]));
    if !is_digit(c) {
        return None;
    }
    let mut back = 0u32;
    let mut code = c as u32;
    while code > 0 {
        code -= 1;
        match char::from_u32(code) {
            Some(prev) if is_digit(prev) => back += 1,
            _ => break,
        }
    }
    Some(back % 10)
}

/// `_number`: an ordinal word, a number word or a numeral ("3rd", "12") as its value.
fn number(word: &str) -> Option<u32> {
    let word = casefold(word);
    if let Some((_, n)) = ORDINALS.iter().find(|(k, _)| *k == word) {
        return *n;
    }
    if let Some((_, n)) = NUMBERS.iter().find(|(k, _)| *k == word) {
        return Some(*n);
    }
    let mut value = 0u32;
    let mut any = false;
    for c in word.chars() {
        match digit_value(c) {
            Some(d) => {
                value = value.saturating_mul(10).saturating_add(d);
                any = true;
            }
            None => break,
        }
    }
    any.then_some(value)
}

/// `_ordinal`: the result number the words say, from the first of the four patterns that matches.
fn ordinal(text: &str) -> Option<u32> {
    for pattern in [&*ORDINAL_RE, &*NUMBER_RE, &*BARE_RE, &*ALONE_RE] {
        if let Some(caps) = captures(pattern, text) {
            let word = caps.iter().skip(1).flatten().map(|g| g.as_str()).find(|g| !g.is_empty())?;
            return number(word);
        }
    }
    None
}

/// Site, query and result number, read from the words alone. Each is None when the request does not say it.
pub fn literal_arguments(request: &str) -> Arguments {
    let text = join_site_names(&squash(request));
    let n = ordinal(&sub_empty(&DISFLUENCY, &text));
    Arguments { site: mentioned_site(&text), query: query(&text), n: n.filter(|n| *n > 0) }
}

// Spoken navigation verbs that mean "open" when what follows is a site or a result ("go to YouTube", "pull up
// Netflix", "click on the fourth link"). Measured on the pinned checkpoint, Laya reads "open" reliably and these
// poorly, so the goal it is shown says "open" in their place. Only a one-step request is rewritten: when a second
// step follows ("pull up Netflix and log in"), Laya is shown the words as said, because it reads "open X and ..."
// as plain open_site and would lose the extra step. Nothing else in the request changes.
const NAV_VERB: &str = r"(?:go\s+(?:to|into|in\s+to|on|onto|with)|head\s+(?:over\s+)?to|take\s+me\s+to|bring\s+me\s+to|navigate\s+to|jump\s+to|hop\s+on(?:to)?|pull\s+up|bring\s+up|load\s+up|load|visit|launch|click\s+on|click|tap\s+on|tap|select|choose|pick)";
/// open a result, never a site ("play the second")
const RESULT_VERB: &str = r"(?:play|watch|listen\s+to|show\s+me|read)";
static NAV_RE: LazyLock<Regex> = LazyLock::new(|| {
    let result_ref = format!(r"(?:the\s+)?(?:(?:{})\b|(?:number|#)\s*(?:{})\b)", *ORD_ALT, *NUM_ALT);
    let site_ref = format!(r"(?:the\s+)?(?:{}){DOMAIN}(?![\w])", TABLE.name_alt);
    re(&format!(r"(?i)\b(?:{NAV_VERB}(?=\s+(?:{site_ref}|{result_ref}))|{RESULT_VERB}(?=\s+{result_ref}))"))
});
static NEXT_STEP: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?i)\b(?:and|then|after\s+that|also)\b|[,;]").expect("next step"));

/// The request as Laya is shown it: disfluencies dropped; in a one-step request, a navigation verb before a site
/// or a result said as "open", and a bare result reference ("the sixth", "number two") said as "open the sixth".
/// The literal arguments are always read from the request itself, not from this.
pub fn laya_goal(text: &str) -> String {
    let goal = squash(&sub_empty(&DISFLUENCY, text));
    if find(&ALONE_RE, &goal).is_some() {
        return format!("open {}", strip_chars(&goal, " .,!?"));
    }
    let Some(m) = find(&NAV_RE, &goal) else { return goal };
    if NEXT_STEP.find_at(&goal, m.end()).is_some() {
        return goal;
    }
    format!("{}open{}", &goal[..m.start()], &goal[m.end()..])
}

// ---- the outcome ---------------------------------------------------------------------------------------------

pub const KINDS: [&str; 4] = ["search", "open_site", "open_result", "none"];

/// One of [`KINDS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Search,
    OpenSite,
    OpenResult,
    None,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Search => "search",
            Kind::OpenSite => "open_site",
            Kind::OpenResult => "open_result",
            Kind::None => "none",
        }
    }

    pub fn parse(kind: &str) -> Option<Kind> {
        match kind {
            "search" => Some(Kind::Search),
            "open_site" => Some(Kind::OpenSite),
            "open_result" => Some(Kind::OpenResult),
            "none" => Some(Kind::None),
            _ => None,
        }
    }
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a request asks for. `latency_ms` is not part of equality.
#[derive(Debug, Clone)]
pub struct RequestOutcome {
    /// exactly one of KINDS
    pub kind: Kind,
    pub site_host: Option<String>,
    pub query: Option<String>,
    pub n: Option<u32>,
    pub confidence: f64,
    /// "laya" or "code"
    pub source: &'static str,
    pub latency_ms: f64,
}

impl RequestOutcome {
    pub fn new(
        kind: Kind,
        site_host: Option<&str>,
        query: Option<&str>,
        n: Option<u32>,
        confidence: f64,
        source: &'static str,
    ) -> Self {
        RequestOutcome {
            kind,
            site_host: site_host.map(str::to_string),
            query: query.map(str::to_string),
            n,
            confidence,
            source,
            latency_ms: 0.0,
        }
    }
}

impl PartialEq for RequestOutcome {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.site_host == other.site_host
            && self.query == other.query
            && self.n == other.n
            && self.confidence == other.confidence
            && self.source == other.source
    }
}

// LayaBrowse's own outcome descriptions and rules (goal_engine.objective_descriptions and the "objective"
// question, MIT, Copyright (c) 2026 Aryan Bhujade), so the checkpoint is asked in the shape it was trained on.
pub const OPTIONS: &[(&str, &str)] = &[
    ("open_site", "Open the requested website. No search or result opening."),
    ("search", "Search for the requested topic and show the results."),
    ("open_result", "Open a particular video, article, repository or item from search results."),
    ("unsupported", "Not a command, ambiguous, or these outcomes omit part of the request"),
];
pub const OPTION_KIND: &[(&str, Kind)] = &[
    ("open_site", Kind::OpenSite),
    ("search", Kind::Search),
    ("open_result", Kind::OpenResult),
    ("unsupported", Kind::None),
];
pub const RULES: &str = "Choose the outcome requested by the speaker, not the next action. \
Include ALL requested work, including opening a result. \
Unrelated speech, unsupported actions or missing details mean unsupported.";
pub const GATE_TOP: f64 = 0.50;
pub const GATE_MARGIN: f64 = 0.10;

pub const BROWSER_MODEL: &str = "cklxx/laya-browser";
pub const BROWSER_REVISION: &str = "8a7e625b481d292d830acf22a2feec81554575fa";
pub const BROWSER_SUBFOLDER: &str = "v10s";
/// MLX keeps freed GPU buffers for reuse and, uncapped, grew past 1.8 GB in a mostly idle background process
/// (LayaBrowse laya.py); 64 MB costs about a millisecond a decision.
pub const MLX_CACHE_LIMIT_BYTES: u64 = 64 * 1024 * 1024;

pub const MODES: [&str; 2] = ["laya", "off"];
static LATENCIES: LazyLock<Mutex<VecDeque<f64>>> = LazyLock::new(|| Mutex::new(VecDeque::with_capacity(256)));

/// The option key ("unsupported" for none) that names a kind.
pub fn option_key(kind: Kind) -> &'static str {
    OPTION_KIND.iter().find(|(_, k)| *k == kind).map(|(key, _)| *key).expect("every kind has an option")
}

fn option_text(key: &str) -> &'static str {
    OPTIONS.iter().find(|(k, _)| *k == key).map(|(_, text)| *text).expect("known option")
}

/// The environment a call reads: the process environment, or an explicit map (tests, callers with their own).
fn env_get(environ: Option<&HashMap<String, String>>, key: &str) -> Option<String> {
    match environ {
        Some(map) => map.get(key).cloned(),
        None => std::env::var(key).ok(),
    }
}

/// EVE_BROWSER_INTENT: "laya" (the default) or "off"; anything else is "off".
pub fn mode(environ: Option<&HashMap<String, String>>) -> &'static str {
    let raw = env_get(environ, "EVE_BROWSER_INTENT").filter(|v| !v.is_empty()).unwrap_or_else(|| "laya".into());
    let value = casefold(py_strip(&raw));
    MODES.iter().find(|m| **m == value).copied().unwrap_or("off")
}

/// Milliseconds per Laya call, newest last (the last 256).
pub fn recent_latencies() -> Vec<f64> {
    LATENCIES.lock().iter().copied().collect()
}

fn record_latency(ms: f64) {
    let mut latencies = LATENCIES.lock();
    if latencies.len() == 256 {
        latencies.pop_front();
    }
    latencies.push_back(ms);
}

/// The engine declines to answer: no model, a malformed answer, under the gate. Never a crash in routing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Abstain(pub String);

/// `type(v) in {int, float} and math.isfinite(v) and 0 <= v <= 1`, as f64.
fn probability(value: &Value) -> Option<f64> {
    let v = value.as_f64()?;
    (v.is_finite() && (0.0..=1.0).contains(&v)).then_some(v)
}

/// Python's `sum()` over floats, which (3.12 and later, the bridge runs 3.13) adds with Neumaier compensation.
fn py_fsum(values: &[f64]) -> f64 {
    let mut total = 0.0f64;
    let mut compensation = 0.0f64;
    for &x in values {
        let t = total + x;
        if total.abs() >= x.abs() {
            compensation += (total - t) + x;
        } else {
            compensation += (x - t) + total;
        }
        total = t;
    }
    if compensation != 0.0 && compensation.is_finite() {
        total + compensation
    } else {
        total
    }
}

/// (option, top probability) when the answer is a well-formed choice that clears the gate, else Abstain.
/// Mirrors LayaBrowse goal_engine.validate_choice: schema integrity, then 0.50 top and 0.10 margin.
pub fn validate_choice(answer: &Value, options: &[(&str, &str)]) -> Result<(String, f64), Abstain> {
    let malformed = || Abstain("malformed answer".into());
    let probabilities = answer.get("probabilities").and_then(Value::as_object).ok_or_else(malformed)?;
    let choice = answer.get("choice").and_then(Value::as_str).ok_or_else(malformed)?;
    let values: Vec<f64> = probabilities.values().map(probability).collect::<Option<_>>().ok_or_else(malformed)?;
    let option_keys: HashSet<&str> = options.iter().map(|(k, _)| *k).collect();
    let answered: HashSet<&str> = probabilities.keys().map(String::as_str).collect();
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let valid = option_keys.contains(choice)
        && answered == option_keys
        && !values.is_empty()
        && (py_fsum(&values) - 1.0).abs() < 0.02
        && probabilities.get(choice).and_then(Value::as_f64).is_some_and(|p| p >= max - 0.0002);
    if !valid {
        return Err(malformed());
    }
    let mut ordered = values.clone();
    ordered.sort_by(|a, b| b.total_cmp(a));
    if ordered[0] < GATE_TOP || (ordered.len() > 1 && ordered[0] - ordered[1] < GATE_MARGIN) {
        return Err(Abstain("under the gate".into()));
    }
    let p = probabilities.get(choice).and_then(Value::as_f64).unwrap_or(0.0);
    Ok((choice.to_string(), p))
}

pub const IDLE_DEFAULT_S: f64 = 600.0;

/// Python's `float()` on an already-stripped string: Rust's float grammar plus digit-separating underscores and
/// non-ASCII decimal digits (`float("١٢")` is 12.0).
fn py_float(raw: &str) -> Option<f64> {
    let ascii: String = raw
        .chars()
        .map(|c| match digit_value(c) {
            Some(d) if !c.is_ascii_digit() => char::from_digit(d, 10).unwrap_or(c),
            _ => c,
        })
        .collect();
    let raw = ascii.as_str();
    let cleaned = if raw.contains('_') {
        let chars: Vec<char> = raw.chars().collect();
        for (i, c) in chars.iter().enumerate() {
            if *c == '_' {
                let before = i > 0 && chars[i - 1].is_ascii_digit();
                let after = chars.get(i + 1).is_some_and(|c| c.is_ascii_digit());
                if !(before && after) {
                    return None;
                }
            }
        }
        raw.replace('_', "")
    } else {
        raw.to_string()
    };
    cleaned.parse::<f64>().ok()
}

/// How long Laya stays loaded with no question before it is unloaded (EVE_BROWSER_INTENT_IDLE_S, default 600).
/// 0 keeps it loaded for the life of the process; anything unreadable is the default.
pub fn idle_seconds(environ: Option<&HashMap<String, String>>) -> f64 {
    let raw = env_get(environ, "EVE_BROWSER_INTENT_IDLE_S").unwrap_or_default();
    let raw = py_strip(&raw);
    let value = if raw.is_empty() {
        IDLE_DEFAULT_S
    } else {
        match py_float(raw) {
            Some(v) => v,
            None => return IDLE_DEFAULT_S,
        }
    };
    if value.is_finite() && value >= 0.0 {
        value
    } else {
        IDLE_DEFAULT_S
    }
}

// ---- the laya-mlx boundary -----------------------------------------------------------------------------------

/// Why the model could not be loaded. The engine remembers it and abstains with the same words every call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    /// laya-mlx (or MLX, or the interpreter that has them) is missing.
    Unavailable(String),
    /// The pinned checkpoint is not in the Hugging Face cache.
    NotCached,
    /// The checkpoint is cached but will not load.
    LoadFailed(String),
}

/// What loads the model. The default is [`LayaSubprocess`]; tests give their own.
pub trait LayaRuntime: Send + Sync {
    fn load(&self) -> Result<Box<dyn LayaAgent>, LoadError>;
}

/// A loaded laya-mlx agent.
pub trait LayaAgent: Send {
    /// The question and its options fit the checkpoint's head budget (LayaBrowse goal_engine._prefix_fits).
    fn prefix_fits(&mut self, question: &Value) -> anyhow::Result<bool>;
    /// `agent.predict(state, questions)`.
    fn predict(&mut self, state: &Value, questions: &Value) -> anyhow::Result<Value>;
    /// False once the agent can no longer answer (its process died); the engine then drops it and reloads.
    fn alive(&mut self) -> bool {
        true
    }
    /// Drop the model and free what it holds.
    fn release(self: Box<Self>);
}

const HELPER: &str = include_str!("laya_helper.py");
const PYTHON_ENV: &str = "EVE_BROWSER_INTENT_PYTHON";

/// The Python that has laya-mlx: EVE_BROWSER_INTENT_PYTHON, else the bridge checkout's venv, else `python3`.
fn default_python() -> PathBuf {
    if let Some(p) = std::env::var_os(PYTHON_ENV).filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    let venv = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/.venv/bin/python3"));
    if venv.exists() {
        return venv;
    }
    PathBuf::from("python3")
}

/// laya-mlx in a helper Python process (`src/laya_helper.py`): loading spawns it, unloading kills it, so the
/// 1.1 GB model and MLX's buffer cache go with the process.
#[derive(Debug, Clone)]
pub struct LayaSubprocess {
    pub python: PathBuf,
    /// Extra environment for the helper (tests point PYTHONPATH at fake modules).
    pub env: Vec<(OsString, OsString)>,
}

impl Default for LayaSubprocess {
    fn default() -> Self {
        LayaSubprocess { python: default_python(), env: Vec::new() }
    }
}

struct HelperProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    dead: bool,
}

impl HelperProcess {
    fn read(&mut self) -> anyhow::Result<Value> {
        let mut line = String::new();
        if self.stdout.read_line(&mut line)? == 0 {
            let status = self.child.try_wait().ok().flatten();
            anyhow::bail!(
                "laya helper exited{}",
                status.map(|s| format!(" ({s})")).unwrap_or_default()
            );
        }
        Ok(serde_json::from_str(line.trim_end())?)
    }

    fn call(&mut self, request: &Value) -> anyhow::Result<Value> {
        let io = (|| -> anyhow::Result<Value> {
            writeln!(self.stdin, "{request}")?;
            self.stdin.flush()?;
            self.read()
        })();
        let reply = io.inspect_err(|_| self.dead = true)?;
        if let Some(error) = reply.get("error") {
            anyhow::bail!("{}", error.as_str().map(str::to_string).unwrap_or_else(|| error.to_string()));
        }
        Ok(reply)
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for HelperProcess {
    fn drop(&mut self) {
        self.kill();
    }
}

impl LayaAgent for HelperProcess {
    fn prefix_fits(&mut self, question: &Value) -> anyhow::Result<bool> {
        let reply = self.call(&json!({"op": "fits", "question": question}))?;
        Ok(reply.get("fits").and_then(Value::as_bool).unwrap_or(false))
    }

    fn predict(&mut self, state: &Value, questions: &Value) -> anyhow::Result<Value> {
        let mut reply = self.call(&json!({"op": "predict", "state": state, "questions": questions}))?;
        Ok(reply.get_mut("result").map(Value::take).unwrap_or(Value::Null))
    }

    fn alive(&mut self) -> bool {
        !self.dead && matches!(self.child.try_wait(), Ok(None))
    }

    fn release(self: Box<Self>) {
        drop(self);
    }
}

impl LayaSubprocess {
    fn command(&self, mode: &str) -> Command {
        let mut cmd = Command::new(&self.python);
        cmd.arg("-c").arg(HELPER).args(self.args(mode));
        cmd.envs(self.env.iter().map(|(k, v)| (k, v)));
        // The script's own directory is not on sys.path; a neutral cwd keeps a planted module from loading.
        cmd.current_dir("/");
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());
        cmd
    }

    /// The helper's arguments after `-c <script>`.
    pub fn args(&self, mode: &str) -> Vec<String> {
        let mut args = vec![mode.to_string(), BROWSER_MODEL.into(), BROWSER_REVISION.into(), BROWSER_SUBFOLDER.into()];
        if mode == "serve" {
            args.push(MLX_CACHE_LIMIT_BYTES.to_string());
        }
        args
    }

    fn spawn(&self, mode: &str) -> Result<HelperProcess, LoadError> {
        let mut child = self.command(mode).spawn().map_err(|e| LoadError::Unavailable(e.to_string()))?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
        Ok(HelperProcess { child, stdin, stdout, dead: false })
    }

    /// `_cached_checkpoint`: the pinned checkpoint's local snapshot, found without the network; None when it is
    /// not downloaded (or there is no huggingface_hub).
    pub fn cached_checkpoint(&self) -> Option<PathBuf> {
        let mut helper = self.spawn("locate").ok()?;
        let status = helper.read().ok()?;
        (status.get("status").and_then(Value::as_str) == Some("cached"))
            .then(|| status.get("path").and_then(Value::as_str).map(PathBuf::from))
            .flatten()
    }
}

impl LayaRuntime for LayaSubprocess {
    fn load(&self) -> Result<Box<dyn LayaAgent>, LoadError> {
        let mut helper = self.spawn("serve")?;
        let status = helper.read().map_err(|e| LoadError::LoadFailed(e.to_string()))?;
        let error = || status.get("error").and_then(Value::as_str).unwrap_or("").to_string();
        match status.get("status").and_then(Value::as_str) {
            Some("ready") => Ok(Box::new(helper)),
            Some("unavailable") => Err(LoadError::Unavailable(error())),
            Some("not_cached") => Err(LoadError::NotCached),
            Some("load_failed") => Err(LoadError::LoadFailed(error())),
            _ => Err(LoadError::LoadFailed(format!("laya helper said {status}"))),
        }
    }
}

/// The pinned checkpoint's local snapshot, found without the network; None when it is not downloaded.
pub fn cached_checkpoint() -> Option<PathBuf> {
    LayaSubprocess::default().cached_checkpoint()
}

// ---- the engine ----------------------------------------------------------------------------------------------

/// Answers `ask(goal, question, state) -> laya-mlx choice answer`. An [`Abstain`] error abstains quietly; any other
/// error is logged and abstains too.
pub trait IntentAsker: Send + Sync {
    fn ask(&self, goal: &str, question: &Value, state: &Value) -> anyhow::Result<Value>;
}

/// Seconds on a monotonic clock.
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

fn monotonic() -> f64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_secs_f64()
}

struct TimerHandle {
    id: u64,
    _cancel: mpsc::Sender<()>,
}

struct EngineState {
    agent: Option<Box<dyn LayaAgent>>,
    failed: String,
    timer: Option<TimerHandle>,
    timer_seq: u64,
    last_used: f64,
}

struct EngineInner {
    state: Mutex<EngineState>,
    idle_s: f64,
    clock: Clock,
    use_timer: bool,
    runtime: Box<dyn LayaRuntime>,
}

/// How a [`LayaKindEngine`] is built; `Default` is the daemon's: idle from the environment, a monotonic clock, the
/// idle timer on, laya-mlx in a helper process.
pub struct EngineOptions {
    pub idle_s: Option<f64>,
    pub clock: Option<Clock>,
    pub timer: bool,
    pub runtime: Option<Box<dyn LayaRuntime>>,
}

impl Default for EngineOptions {
    fn default() -> Self {
        EngineOptions { idle_s: None, clock: None, timer: true, runtime: None }
    }
}

/// One process-local laya-mlx agent, loaded on first use. A failed load is remembered, so a missing checkpoint
/// costs one attempt, not one per request. Predictions are serialised under one lock.
///
/// The loaded model holds about 1.1 GB, so after `idle_s` seconds with no question it is dropped and MLX's buffer
/// cache cleared; the next question loads it again (about 0.4 s). A timer thread does the dropping; `timer: false`
/// leaves it to [`LayaKindEngine::unload_if_idle`], which tests call with their own clock.
pub struct LayaKindEngine {
    inner: Arc<EngineInner>,
}

impl Default for LayaKindEngine {
    fn default() -> Self {
        Self::new(EngineOptions::default())
    }
}

impl LayaKindEngine {
    pub fn new(options: EngineOptions) -> Self {
        let idle_s = match options.idle_s {
            None => idle_seconds(None),
            Some(v) => v.max(0.0),
        };
        LayaKindEngine {
            inner: Arc::new(EngineInner {
                state: Mutex::new(EngineState {
                    agent: None,
                    failed: String::new(),
                    timer: None,
                    timer_seq: 0,
                    last_used: 0.0,
                }),
                idle_s,
                clock: options.clock.unwrap_or_else(|| Arc::new(monotonic)),
                use_timer: options.timer,
                runtime: options.runtime.unwrap_or_else(|| Box::new(LayaSubprocess::default())),
            }),
        }
    }

    pub fn loaded(&self) -> bool {
        self.inner.state.lock().agent.is_some()
    }

    pub fn warm(&self) -> Result<(), Abstain> {
        let mut state = self.inner.state.lock();
        self.inner.load_locked(&mut state)?;
        EngineInner::touch_locked(&self.inner, &mut state);
        Ok(())
    }

    /// Drop the model when it has been idle for `idle_s`; true when it was dropped.
    pub fn unload_if_idle(&self) -> bool {
        let mut state = self.inner.state.lock();
        if state.agent.is_none()
            || self.inner.idle_s <= 0.0
            || (self.inner.clock)() - state.last_used < self.inner.idle_s
        {
            return false;
        }
        self.inner.drop_locked(&mut state);
        true
    }

    /// Drop the model now, and stop the idle timer.
    pub fn unload(&self) {
        let mut state = self.inner.state.lock();
        state.timer = None;
        if state.agent.is_some() {
            self.inner.drop_locked(&mut state);
        }
    }
}

impl EngineInner {
    fn load_locked(&self, state: &mut EngineState) -> Result<(), Abstain> {
        if state.agent.is_some() {
            return Ok(());
        }
        if !state.failed.is_empty() {
            return Err(Abstain(state.failed.clone()));
        }
        match self.runtime.load() {
            Ok(agent) => {
                state.agent = Some(agent);
                Ok(())
            }
            Err(LoadError::Unavailable(exc)) => {
                state.failed = format!("laya-mlx unavailable: {exc}");
                Err(Abstain(state.failed.clone()))
            }
            Err(LoadError::NotCached) => {
                // Never download from inside a request; the checkpoint is fetched once, by hand.
                state.failed = format!("{BROWSER_MODEL}@{} is not cached", &BROWSER_REVISION[..7]);
                Err(Abstain(state.failed.clone()))
            }
            Err(LoadError::LoadFailed(exc)) => {
                state.failed = format!("load failed: {exc}");
                tracing::warn!("browser intent: {}", state.failed);
                Err(Abstain(state.failed.clone()))
            }
        }
    }

    fn touch_locked(this: &Arc<EngineInner>, state: &mut EngineState) {
        state.last_used = (this.clock)();
        if this.use_timer && this.idle_s > 0.0 && state.timer.is_none() {
            Self::schedule_locked(this, state, this.idle_s);
        }
    }

    fn schedule_locked(this: &Arc<EngineInner>, state: &mut EngineState, delay: f64) {
        state.timer_seq += 1;
        let id = state.timer_seq;
        let (cancel, cancelled) = mpsc::channel::<()>();
        let weak: Weak<EngineInner> = Arc::downgrade(this);
        // An idle period too long for a Duration (an explicit infinity) never fires, as Python's Timer thread
        // dies on it: the channel then blocks until the handle is dropped.
        let delay = Duration::try_from_secs_f64(delay.max(0.0)).unwrap_or(Duration::MAX);
        let spawned = std::thread::Builder::new().name("browser-intent-idle".into()).spawn(move || {
            // A send or a dropped handle (unload, a newer timer) cancels; only a timeout fires.
            if let Err(mpsc::RecvTimeoutError::Timeout) = cancelled.recv_timeout(delay) {
                if let Some(inner) = weak.upgrade() {
                    EngineInner::on_timer(&inner, id);
                }
            }
        });
        if spawned.is_ok() {
            state.timer = Some(TimerHandle { id, _cancel: cancel });
        }
    }

    fn on_timer(this: &Arc<EngineInner>, id: u64) {
        let mut state = this.state.lock();
        if state.timer.as_ref().map(|t| t.id) != Some(id) {
            return;
        }
        state.timer = None;
        if state.agent.is_none() {
            return;
        }
        let remaining = state.last_used + this.idle_s - (this.clock)();
        if remaining > 0.0 {
            Self::schedule_locked(this, &mut state, remaining);
            return;
        }
        this.drop_locked(&mut state);
    }

    fn drop_locked(&self, state: &mut EngineState) {
        if let Some(agent) = state.agent.take() {
            agent.release();
        }
        tracing::info!("browser intent: Laya unloaded after {:.0} s idle", self.idle_s);
    }
}

impl IntentAsker for LayaKindEngine {
    fn ask(&self, _goal: &str, question: &Value, state_json: &Value) -> anyhow::Result<Value> {
        let inner = &self.inner;
        let mut state = inner.state.lock();
        inner.load_locked(&mut state)?;
        EngineInner::touch_locked(inner, &mut state);
        let agent = state.agent.as_mut().expect("loaded");
        let fits = agent.prefix_fits(question);
        let result = match fits {
            Ok(true) => agent.predict(state_json, &json!({"objective": question})),
            Ok(false) => Err(Abstain("request exceeds the checkpoint's question budget".into()).into()),
            Err(e) => Err(e),
        };
        if result.is_err() && !agent.alive() {
            // The helper process died: drop it, so the next question loads a fresh one.
            if let Some(agent) = state.agent.take() {
                agent.release();
            }
        }
        let result = result?;
        EngineInner::touch_locked(inner, &mut state);
        drop(state);
        let Value::Object(result) = result else { return Ok(json!({})) };
        match result.get("answers") {
            None => Ok(json!({})),
            Some(answers) if !truthy(answers) => Ok(json!({})),
            Some(Value::Object(answers)) => Ok(answers.get("objective").cloned().unwrap_or_else(|| json!({}))),
            Some(other) => anyhow::bail!("'{}' object has no attribute 'get'", py_type(other)),
        }
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn py_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

static ENGINE: OnceLock<LayaKindEngine> = OnceLock::new();

/// The process's one Laya engine, built on first use.
pub fn shared_engine() -> &'static LayaKindEngine {
    ENGINE.get_or_init(LayaKindEngine::default)
}

/// The options Laya is shown: only the kinds these arguments could carry, then "unsupported" — the way
/// LayaBrowse's contract_options lets a spoken query veto opening the bare site and a spoken result number veto a
/// plain search, so Laya never weighs an outcome that would drop part of the request.
pub fn offered(args: &Arguments) -> Vec<(&'static str, &'static str)> {
    let mut out: Vec<(&'static str, &'static str)> = [Kind::OpenSite, Kind::Search, Kind::OpenResult]
        .into_iter()
        .filter(|kind| {
            (*kind == Kind::OpenResult && (args.n.is_some() || args.query.as_deref().is_some_and(|q| !q.is_empty())))
                || carried(*kind, args)
        })
        .map(|kind| {
            let key = option_key(kind);
            (key, option_text(key))
        })
        .collect();
    out.push(("unsupported", option_text("unsupported")));
    out
}

/// `_carried`: can the literal arguments carry this kind, with no part of the request dropped?
fn carried(kind: Kind, args: &Arguments) -> bool {
    let has_query = args.query.as_deref().is_some_and(|q| !q.is_empty());
    match kind {
        Kind::Search => has_query && args.n.is_none(),
        Kind::OpenSite => args.site.is_some() && !has_query && args.n.is_none(),
        Kind::OpenResult => args.n.is_some(),
        Kind::None => true,
    }
}

/// Python's `round(x, 4)`: correctly rounded through the shortest decimal.
fn round4(x: f64) -> f64 {
    format!("{x:.4}").parse().unwrap_or(x)
}

/// The outcome the request asks for, or None to abstain (crate docs). `engine` answers
/// `ask(goal, question, state) -> laya-mlx choice answer`; the default is the shared local Laya. `environ` is the
/// process environment when None.
pub fn expected_outcome(
    request: &str,
    engine: Option<&dyn IntentAsker>,
    environ: Option<&HashMap<String, String>>,
) -> Option<RequestOutcome> {
    if mode(environ) != "laya" {
        return None;
    }
    let text = join_site_names(&squash(request));
    if text.is_empty() {
        return None;
    }
    let args = literal_arguments(&text);
    if args.site.is_none() && args.query.is_none() && args.n.is_none() {
        // Nothing to search for, no site and no result: none of the three can be carried out.
        return Some(RequestOutcome::new(Kind::None, None, None, None, 1.0, "code"));
    }
    let asker: &dyn IntentAsker = match engine {
        Some(engine) => engine,
        None => shared_engine(),
    };
    let options = offered(&args);
    let goal = laya_goal(&text);
    let criteria: Map<String, Value> = options.iter().map(|(k, v)| (k.to_string(), json!(v))).collect();
    let question = json!({"type": "choice", "instructions": {"goal": goal, "rules": RULES}, "criteria": criteria});
    // No page yet: the outcome depends on the words. Same keys as LayaBrowse's goal state.
    let state = json!({"page": {"url": "about:blank", "title": "", "text": ""}, "recent_actions": []});
    let started = Instant::now();
    let answered = asker.ask(&goal, &question, &state).map(|answer| {
        let latency = started.elapsed().as_secs_f64() * 1000.0;
        record_latency(latency);
        (answer, latency)
    });
    let (option, confidence, latency) =
        match answered.and_then(|(answer, latency)| Ok(validate_choice(&answer, &options).map(|(o, c)| (o, c, latency))?)) {
            Ok(v) => v,
            Err(exc) => {
                if exc.downcast_ref::<Abstain>().is_some() {
                    tracing::debug!("browser intent abstains: {exc}");
                } else {
                    tracing::warn!("browser intent: {exc}");
                }
                return None;
            }
        };
    let kind = OPTION_KIND.iter().find(|(k, _)| *k == option).map(|(_, kind)| *kind)?;
    if !carried(kind, &args) {
        return None;
    }
    let confidence = round4(confidence);
    if kind == Kind::None {
        return Some(RequestOutcome { latency_ms: latency, ..RequestOutcome::new(Kind::None, None, None, None, confidence, "laya") });
    }
    Some(RequestOutcome {
        kind,
        site_host: args.site.map(|s| s.host.to_string()),
        query: if matches!(kind, Kind::Search | Kind::OpenResult) { args.query.clone() } else { None },
        n: if kind == Kind::OpenResult { args.n } else { None },
        confidence,
        source: "laya",
        latency_ms: latency,
    })
}

#[cfg(test)]
mod tests;
