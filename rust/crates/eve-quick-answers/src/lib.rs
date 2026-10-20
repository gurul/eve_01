//! The standard details, answered by code: no model turn and no web search. A port of `quick_answers.py`.
//!
//! The owner asked on 2026-09-26: "route in standard details like time weather and things like that manually … i
//! dont need it searching unnecessary stuff … look for other things you can feed in as well". A question that is
//! only a standard question needs no judgement, so code answers it at once:
//!
//! | Ask       | Examples                                                                 | Source                    |
//! |-----------|--------------------------------------------------------------------------|---------------------------|
//! | time      | "what time is it", "what time is it in Tokyo"                            | the clock (+ geocoder)    |
//! | date      | "what's the date", "what day is it"                                      | the clock                 |
//! | weather   | "what's the weather", "weather tomorrow", "weather in Paris", "will it rain today", "how cold is it", "do I need an umbrella" | Open-Meteo forecast |
//! | sun       | "when is sunset", "what time is sunrise tomorrow"                        | Open-Meteo forecast       |
//! | air       | "what's the air quality", "air quality in Delhi"                         | Open-Meteo air quality    |
//! | math      | "what's 17 times 23", "15 percent of 80", "2 to the power of 10"         | arithmetic (no eval)      |
//! | convert   | "convert 5 km to miles", "72 fahrenheit in celsius", "how many ounces in a pound" | a unit table     |
//! | countdown | "how many days until christmas"                                          | the calendar              |
//! | battery   | "what's my battery", "how much battery do I have"                        | the Mac's pmset           |
//!
//! A question is answered here only when it is **only** that question (`QuickAnswers::match_text`): "what time is
//! my meeting", "should I bike given the weather" and "what's 17 times my rent" go to the model, which has the
//! calendar, the search and the judgement. When a source fails, `answer` returns None and the model answers as
//! before, so a failure here costs nothing. Open-Meteo is free and needs no key.
//!
//! Where "here" is: the phone's own location while it is fresh (`eve_places::phone_location`: the Eve Link app
//! shared a fix at most `PHONE_FRESH_MIN` minutes ago; its coordinates go to Open-Meteo rounded to
//! `PHONE_DECIMALS` decimals, about a kilometer), else `EVE_WEATHER_PLACE` (a place name, looked up once with
//! Open-Meteo's geocoder), or `EVE_WEATHER_LAT` and `EVE_WEATHER_LON`. Without any of them, questions about "here"
//! (weather, sun, air) go to the model. The time zone is `EVE_TIMEZONE` (an IANA name), as system_context reads
//! it, else the Mac's.
//!
//! Coordinates never reach the log (`CoordinateFilter`): the Python daemon's httpx logs each request line at INFO
//! with its query string, so every Open-Meteo call would write where the phone was, a location history in a plain
//! file. This port logs the same request line (target "httpx") and passes it through the filter, which replaces
//! the latitude and longitude values in any Open-Meteo URL.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use chrono::{Datelike, NaiveDate, NaiveDateTime, Utc};
use chrono_tz::Tz;
use futures::future::BoxFuture;
use num_bigint::BigInt;
use num_traits::{Signed, ToPrimitive, Zero};
use parking_lot::Mutex;
use regex::Regex;
use serde_json::{json, Map, Value};

pub use eve_places::Env;
use eve_places::{phone_location, py};

pub const FORECAST_URL: &str = "https://api.open-meteo.com/v1/forecast";
pub const AIR_URL: &str = "https://air-quality-api.open-meteo.com/v1/air-quality";
pub const GEOCODE_URL: &str = "https://geocoding-api.open-meteo.com/v1/search";
pub const TIMEOUT_SECS: f64 = 5.0;
/// phone_location::STALE_MIN: an older fix is not "here" any more.
pub const PHONE_FRESH_MIN: i64 = 60;
/// What of a phone fix Open-Meteo is sent: about a kilometer.
pub const PHONE_DECIMALS: usize = 2;
/// A phone fix with no place name.
pub const PHONE_PLACE: &str = "where you are";
/// What the log shows for a coordinate.
pub const HIDDEN: &str = "<hidden>";
pub const MAX_EXPONENT: i64 = 64;

// ---- Python's regex dialect ---------------------------------------------------------------------------------------

/// A Python `re` pattern for the `regex` crate: `\s` there also matches the four separators `\x1c`..`\x1f`
/// (`str.isspace`), which Unicode's White_Space does not.
fn pyre(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len() + 16);
    let mut in_class = false;
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('s') => out.push_str(if in_class { r"\s\x1c-\x1f" } else { r"[\s\x1c-\x1f]" }),
                Some(n) => {
                    out.push('\\');
                    out.push(n);
                }
                None => out.push('\\'),
            }
            continue;
        }
        if c == '[' && !in_class {
            in_class = true;
        } else if c == ']' && in_class {
            in_class = false;
        }
        out.push(c);
    }
    out
}

/// `re.compile(p)` for `.search` / `.sub`.
fn re(pattern: &str) -> Regex {
    Regex::new(&pyre(pattern)).expect("a valid pattern")
}

/// `re.compile(p)` for `.fullmatch`.
fn full(pattern: &str) -> Regex {
    Regex::new(&format!("^(?:{})$", pyre(pattern))).expect("a valid pattern")
}

fn fancy(pattern: &str) -> fancy_regex::Regex {
    fancy_regex::Regex::new(&pyre(pattern)).expect("a valid pattern")
}

fn group<'t>(caps: &regex::Captures<'t>, name: &str) -> Option<&'t str> {
    caps.name(name).map(|m| m.as_str())
}

/// `s.strip(chars)`.
fn strip_chars<'a>(s: &'a str, chars: &str) -> &'a str {
    s.trim_matches(|c| chars.contains(c))
}

/// `s[:1].upper() + s[1:]`.
fn upper_first(s: &str) -> String {
    let mut it = s.chars();
    match it.next() {
        Some(c) => c.to_uppercase().chain(it).collect(),
        None => String::new(),
    }
}

// ---- the log ------------------------------------------------------------------------------------------------------

static COORD: LazyLock<Regex> = LazyLock::new(|| re(r#"\b((?:latitude|longitude)=)[^&\s"']+"#));

/// Rewrites a log line that quotes an Open-Meteo URL so its latitude/longitude values read `HIDDEN`. It sits on
/// the request lines logged under the target "httpx" (the logger Python's filter sits on).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CoordinateFilter;

impl CoordinateFilter {
    /// The message as it may be logged: unchanged unless it is an Open-Meteo URL with coordinates.
    pub fn filter(&self, message: &str) -> String {
        if message.contains("open-meteo.com") && COORD.is_match(message) {
            COORD.replace_all(message, format!("${{1}}{HIDDEN}").as_str()).into_owned()
        } else {
            message.to_string()
        }
    }
}

/// Whether the "httpx" request lines go through CoordinateFilter. On from the start, as Python's import installs
/// it.
static HTTPX_FILTER: AtomicBool = AtomicBool::new(true);

/// Install CoordinateFilter on the "httpx" request lines, once (idempotent).
pub fn hide_coordinates() {
    HTTPX_FILTER.store(true, Ordering::SeqCst);
}

/// Take the filter off again, as Python's `logger.removeFilter` does (the tests' positive control).
pub fn remove_coordinate_filter() {
    HTTPX_FILTER.store(false, Ordering::SeqCst);
}

/// Log one request line under "httpx", through the filter while it is installed.
fn log_http(line: &str) {
    let line = if HTTPX_FILTER.load(Ordering::SeqCst) { CoordinateFilter.filter(line) } else { line.to_string() };
    tracing::info!(target: "httpx", "{line}");
}

// ---- the question -------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Time,
    Date,
    Weather,
    Sun,
    Air,
    Math,
    Convert,
    Countdown,
    Battery,
}

impl Kind {
    /// Python's `Ask.kind` string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Time => "time",
            Kind::Date => "date",
            Kind::Weather => "weather",
            Kind::Sun => "sun",
            Kind::Air => "air",
            Kind::Math => "math",
            Kind::Convert => "convert",
            Kind::Countdown => "countdown",
            Kind::Battery => "battery",
        }
    }
}

/// A number as Python's arithmetic holds it: an exact int, or a float.
#[derive(Debug, Clone, PartialEq)]
pub enum Num {
    Int(BigInt),
    Float(f64),
}

impl Num {
    /// `float(v)` (an int too big for a float is infinite here, where Python raises OverflowError).
    pub fn to_f64(&self) -> f64 {
        match self {
            Num::Int(i) => i.to_f64().unwrap_or(f64::INFINITY),
            Num::Float(f) => *f,
        }
    }
}

/// `{"said": the question as asked, "value": the result}`.
#[derive(Debug, Clone, PartialEq)]
pub struct Sum {
    pub said: String,
    pub value: Num,
}

/// `{"value", "src", "dst", "result"}`.
#[derive(Debug, Clone, PartialEq)]
pub struct Conversion {
    pub value: f64,
    pub src: String,
    pub dst: String,
    pub result: f64,
}

/// Python's `Ask.args` dict, whose keys depend on the kind.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Args {
    #[default]
    None,
    /// `{"which": "sunrise" | "sunset"}`
    Sun { which: String },
    /// `{"what": the holiday as said}`
    Countdown { what: String },
    Convert(Conversion),
    Math(Sum),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Ask {
    pub kind: Kind,
    /// "" = here
    pub place: String,
    /// 0 = today, 1 = tomorrow
    pub day: usize,
    pub args: Args,
}

impl Ask {
    pub fn new(kind: Kind) -> Self {
        Ask { kind, place: String::new(), day: 0, args: Args::None }
    }

    pub fn with_place(mut self, place: &str) -> Self {
        self.place = place.to_string();
        self
    }

    pub fn with_day(mut self, day: usize) -> Self {
        self.day = day;
        self
    }

    pub fn with_args(mut self, args: Args) -> Self {
        self.args = args;
        self
    }
}

// ---- understanding the question -----------------------------------------------------------------------------------

static LEAD: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:(?:hey|hi|ok|okay|yo)\s+)?eve\b[\s,:]*"));
static POLITE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:(?:can|could) you (?:tell me|check)|do you know|tell me|please tell me|please)\s+")
});
static TAIL: LazyLock<Regex> = LazyLock::new(|| re(r"\s+(?:please|eve|thanks|thank you)$"));
static SPACES: LazyLock<Regex> = LazyLock::new(|| re(r"\s+"));
const PLACE: &str = r"(?P<place>[a-z][a-z .'\-]{1,40}?)";
const WHEN: &str = r"(?P<when> today| now| right now| tomorrow| outside| out)?";

fn when2() -> String {
    WHEN.replace("when", "when2")
}

pub fn normalize(text: &str) -> String {
    let t = text.to_lowercase().replace('\u{2019}', "'");
    let t = LEAD.replace(py::strip(&t), "");
    let t = SPACES.replace_all(&t, " ");
    let t = strip_chars(&t, " ?!.,");
    let t = POLITE.replace(t, "");
    let t = TAIL.replace(&t, "");
    strip_chars(&t, " ?!.,").to_string()
}

fn day_of(when: Option<&str>) -> usize {
    usize::from(when.is_some_and(|w| w.contains("tomorrow")))
}

static TIME: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![full(&format!(
        r"(?:what(?:'s| is)? (?:the )?time(?: is it)?|what time is it|what time it is|the time|time)(?: now| right now)?(?: in {PLACE})?(?: now| right now)?"
    ))]
});
static DATE: LazyLock<Regex> = LazyLock::new(|| {
    full(concat!(
        r"what(?:'s| is)? (?:the )?(?:date|day)(?: today)?(?: is it)?|what(?:'s| is)? today(?:'s date)?",
        r"|what day is (?:it|today)(?: today)?|what date is (?:it|today)|today's date|the date|date"
    ))
});
static WEATHER: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    let w2 = when2();
    vec![
        full(&format!(
            r"(?:(?:what(?:'s| is)?|how(?:'s| is)?) (?:the )?)?weather(?: like| going to be like| forecast)?{WHEN}(?: in {PLACE})?{w2}"
        )),
        full(r"(?:what(?:'s| is)?|how(?:'s| is)?) it (?:like )?(?:outside|out)"),
        full(&format!(r"(?:is it|will it|is it going to|is it gonna) (?:rain|raining|snow|snowing){WHEN}(?: in {PLACE})?")),
        full(&format!(r"(?:what(?:'s| is)? )?(?:the )?temperature(?: outside)?{WHEN}(?: in {PLACE})?")),
        full(&format!(r"how (?:hot|cold|warm) is it(?: outside)?{WHEN}(?: in {PLACE})?")),
        full(&format!(r"do i need (?:an umbrella|a jacket|a coat){WHEN}")),
    ]
});
static SUN: LazyLock<Regex> = LazyLock::new(|| {
    full(&format!(
        r"(?:when(?:'s| is)?|what time(?:'s| is)?) (?:the )?(?P<which>sunrise|sunset){WHEN}|when does the sun (?P<verb>rise|set){}",
        when2()
    ))
});
static AIR: LazyLock<Regex> = LazyLock::new(|| {
    full(&format!(r"(?:(?:what(?:'s| is)?|how(?:'s| is)?) (?:the )?)?(?:air quality|aqi){WHEN}(?: in {PLACE})?"))
});
static BATTERY: LazyLock<Regex> = LazyLock::new(|| {
    full(concat!(
        r"(?:what(?:'s| is)? )?(?:my |the )?(?:mac(?:book)?'?s? |laptop'?s? |computer'?s? )?battery",
        r"(?: level| percentage)?(?: at)?|how much battery (?:do i have|is left|left)(?: left)?",
        r"|how(?:'s| is) (?:my |the )?battery"
    ))
});
static COUNTDOWN: LazyLock<Regex> = LazyLock::new(|| {
    full(r"(?:how many days|how long) (?:is it )?(?:until|till|til|to|before) (?P<what>[a-z' ]{3,30})")
});

/// The standard question these words are, or None when they are anything more. (Python's module-level `match`.)
pub fn match_text(text: &str) -> Option<Ask> {
    let t = normalize(text);
    if t.is_empty() || py::chars_len(&t) > 90 {
        return None;
    }
    for p in TIME.iter() {
        if let Some(m) = p.captures(&t) {
            return Some(Ask::new(Kind::Time).with_place(py::strip(group(&m, "place").unwrap_or(""))));
        }
    }
    if DATE.is_match(&t) {
        return Some(Ask::new(Kind::Date));
    }
    for p in WEATHER.iter() {
        if let Some(m) = p.captures(&t) {
            let when = group(&m, "when").filter(|w| !w.is_empty()).or_else(|| group(&m, "when2"));
            return Some(
                Ask::new(Kind::Weather)
                    .with_place(py::strip(group(&m, "place").unwrap_or("")))
                    .with_day(day_of(when)),
            );
        }
    }
    if let Some(m) = SUN.captures(&t) {
        let which = match group(&m, "which") {
            Some(w) => w.to_string(),
            None if group(&m, "verb") == Some("rise") => "sunrise".to_string(),
            None => "sunset".to_string(),
        };
        let when = group(&m, "when").filter(|w| !w.is_empty()).or_else(|| group(&m, "when2"));
        return Some(Ask::new(Kind::Sun).with_day(day_of(when)).with_args(Args::Sun { which }));
    }
    if let Some(m) = AIR.captures(&t) {
        return Some(Ask::new(Kind::Air).with_place(py::strip(group(&m, "place").unwrap_or(""))));
    }
    if BATTERY.is_match(&t) {
        return Some(Ask::new(Kind::Battery));
    }
    if let Some(m) = COUNTDOWN.captures(&t) {
        let what = py::strip(group(&m, "what").unwrap_or(""));
        let y2k = NaiveDate::from_ymd_opt(2000, 1, 1).expect("a date");
        if holiday(what, y2k).is_some() {
            return Some(Ask::new(Kind::Countdown).with_args(Args::Countdown { what: what.to_string() }));
        }
    }
    if let Some(conv) = conversion(&t) {
        return Some(Ask::new(Kind::Convert).with_args(Args::Convert(conv)));
    }
    if let Some(sum) = arithmetic(&t) {
        return Some(Ask::new(Kind::Math).with_args(Args::Math(sum)));
    }
    None
}

// ---- math: a small evaluator, never eval() ------------------------------------------------------------------------

static MATH_WORDS: LazyLock<Vec<(fancy_regex::Regex, &'static str)>> = LazyLock::new(|| {
    [
        (r"\bmultiplied by\b", "*"),
        (r"\btimes\b", "*"),
        (r"(?<=\d)\s*x\s*(?=\d)", "*"),
        (r"×", "*"),
        (r"\bdivided by\b", "/"),
        (r"\bover\b", "/"),
        (r"÷", "/"),
        (r"\bplus\b", "+"),
        (r"\bminus\b", "-"),
        (r"\bto the power of\b", "**"),
        (r"\braised to\b", "**"),
        (r"\^", "**"),
        (r"\bsquared\b", "**2"),
        (r"\bcubed\b", "**3"),
    ]
    .into_iter()
    .map(|(p, sym)| (fancy(p), sym))
    .collect()
});
static SUM_LEAD: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:what(?:'s| is)|how much is|calculate|compute|solve)\s+"));
static SUM_TAIL: LazyLock<Regex> = LazyLock::new(|| re(r"\s+(?:equals?|is)$"));
static PERCENT: LazyLock<Regex> = LazyLock::new(|| full(r"(-?\d+(?:\.\d+)?) ?(?:%|percent) of (-?\d+(?:\.\d+)?)"));
static ROOT: LazyLock<Regex> = LazyLock::new(|| full(r"(?:the )?square root of (\d+(?:\.\d+)?)"));
static DATE_SLASH: LazyLock<Regex> = LazyLock::new(|| re(r"\d/\d"));
static THOUSANDS: LazyLock<fancy_regex::Regex> = LazyLock::new(|| fancy(r"(?<=\d),(?=\d{3})"));
static SUM_CHARS: LazyLock<Regex> = LazyLock::new(|| full(r"[\d\s.+\-*/()]+"));
static SUM_OP: LazyLock<Regex> = LazyLock::new(|| re(r"\d\s*(?:\*\*|[+\-*/])\s*[\d(]"));

/// One Unicode decimal digit (`\d`, category Nd).
static ND: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d$").expect("a pattern"));

/// The value of a decimal digit: Unicode encodes every Nd digit in runs of ten, 0 to 9, so it is the distance from
/// the start of its run of Nd code points, modulo ten.
fn digit_value(c: char) -> Option<u32> {
    if c.is_ascii_digit() {
        return c.to_digit(10);
    }
    let is_nd = |c: char| ND.is_match(c.encode_utf8(&mut [0u8; 4]));
    if !is_nd(c) {
        return None;
    }
    let mut start = c as u32;
    while let Some(prev) = start.checked_sub(1).and_then(char::from_u32) {
        if !is_nd(prev) {
            break;
        }
        start -= 1;
    }
    Some((c as u32 - start) % 10)
}

/// Python's `float(text)` for the digits these patterns capture: `-?\d+(?:\.\d+)?`, where `\d` and `float()` both
/// take any Unicode decimal digit.
fn digits_float(text: &str) -> Option<f64> {
    let ascii: Option<String> = text
        .chars()
        .map(|c| match c {
            '-' | '.' => Some(c),
            c => digit_value(c).and_then(|d| char::from_digit(d, 10)),
        })
        .collect();
    ascii?.parse::<f64>().ok()
}

/// The sum for words that are only a sum.
pub fn arithmetic(t: &str) -> Option<Sum> {
    let body = SUM_LEAD.replace(t, "");
    let body = SUM_TAIL.replace(&body, "").into_owned();
    if let Some(m) = PERCENT.captures(&body) {
        let a = digits_float(&m[1])?;
        let b = digits_float(&m[2])?;
        return Some(Sum {
            said: format!("{} percent of {}", num_text(&Num::Float(a)), num_text(&Num::Float(b))),
            value: Num::Float(a / 100.0 * b),
        });
    }
    if let Some(m) = ROOT.captures(&body) {
        let a = digits_float(&m[1])?;
        return Some(Sum { said: format!("the square root of {}", num_text(&Num::Float(a))), value: Num::Float(a.sqrt()) });
    }
    if DATE_SLASH.is_match(&body) {
        return None; // "9/11", "24/7": a date or a phrase, not a division
    }
    let mut expr = body.clone();
    for (pattern, sym) in MATH_WORDS.iter() {
        expr = pattern.try_replacen(&expr, 0, format!(" {sym} ").as_str()).ok()?.into_owned();
    }
    expr = THOUSANDS.try_replacen(&expr, 0, "").ok()?.into_owned(); // 1,000
    if !SUM_CHARS.is_match(&expr) || !SUM_OP.is_match(&expr) {
        return None;
    }
    let value = evaluate(py::strip(&expr)).ok()?;
    let finite = match &value {
        Num::Int(i) => i.to_f64().is_some_and(f64::is_finite), // an exact int past the float range has no float
        Num::Float(f) => f.is_finite(),
    };
    if !finite {
        return None;
    }
    Some(Sum { said: body, value })
}

/// Why a sum is not one: Python's SyntaxError, ValueError, ZeroDivisionError, OverflowError and RecursionError,
/// which `arithmetic` all turns into None.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NotArithmetic;

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(Num),
    Plus,
    Minus,
    Star,
    Slash,
    DoubleStar,
    DoubleSlash,
    Open,
    Close,
}

/// Python's tokenizer over the characters a sum may hold: digits, `.`, `+ - * /`, parentheses and blanks.
fn tokenize(src: &str) -> Result<Vec<Tok>, NotArithmetic> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            ' ' | '\t' | '\x0c' => i += 1,
            // a line break joins lines only inside parentheses
            '\n' | '\r' if depth > 0 => i += 1,
            '0'..='9' | '.' => {
                let start = i;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
                let int_end = i;
                let mut is_float = false;
                if i < chars.len() && chars[i] == '.' {
                    if int_end == start && !chars.get(i + 1).is_some_and(char::is_ascii_digit) {
                        return Err(NotArithmetic); // a lone "." (attribute access, Ellipsis): not a number
                    }
                    is_float = true;
                    i += 1;
                    while i < chars.len() && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                let text: String = chars[start..i].iter().collect();
                if is_float {
                    out.push(Tok::Num(Num::Float(text.parse::<f64>().map_err(|_| NotArithmetic)?)));
                } else {
                    // "012": leading zeros in a decimal int are a SyntaxError; "00" is zero
                    if text.len() > 1 && text.starts_with('0') && text.chars().any(|d| d != '0') {
                        return Err(NotArithmetic);
                    }
                    out.push(Tok::Num(Num::Int(text.parse::<BigInt>().map_err(|_| NotArithmetic)?)));
                }
            }
            '+' => {
                out.push(Tok::Plus);
                i += 1;
            }
            '-' => {
                out.push(Tok::Minus);
                i += 1;
            }
            '*' => {
                if chars.get(i + 1) == Some(&'*') {
                    out.push(Tok::DoubleStar);
                    i += 2;
                } else {
                    out.push(Tok::Star);
                    i += 1;
                }
            }
            '/' => {
                if chars.get(i + 1) == Some(&'/') {
                    out.push(Tok::DoubleSlash);
                    i += 2;
                } else {
                    out.push(Tok::Slash);
                    i += 1;
                }
            }
            '(' => {
                depth += 1;
                out.push(Tok::Open);
                i += 1;
            }
            ')' => {
                depth = depth.saturating_sub(1);
                out.push(Tok::Close);
                i += 1;
            }
            _ => return Err(NotArithmetic),
        }
    }
    Ok(out)
}

/// How deep the parser goes before it gives up (Python's nesting and recursion limits).
const MAX_DEPTH: usize = 200;

/// Python's expression grammar over those tokens, evaluated as `_eval` walks the tree: `+ - * /` and `**` on ints
/// and floats, unary `+`/`-`. Anything else (`//`, a call, an empty tuple) is not arithmetic.
struct Parser {
    toks: Vec<Tok>,
    at: usize,
    depth: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.at)
    }

    fn enter(&mut self) -> Result<(), NotArithmetic> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(NotArithmetic);
        }
        Ok(())
    }

    // sum: term (('+' | '-') term)*
    fn sum(&mut self) -> Result<Num, NotArithmetic> {
        self.enter()?;
        let mut left = self.term()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Plus) => Op::Add,
                Some(Tok::Minus) => Op::Sub,
                _ => break,
            };
            self.at += 1;
            let right = self.term()?;
            left = binop(op, left, right)?;
        }
        self.depth -= 1;
        Ok(left)
    }

    // term: factor (('*' | '/' | '//') factor)*
    fn term(&mut self) -> Result<Num, NotArithmetic> {
        let mut left = self.factor()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Star) => Op::Mul,
                Some(Tok::Slash) => Op::Div,
                Some(Tok::DoubleSlash) => return Err(NotArithmetic), // FloorDiv is not one of _OPS
                _ => break,
            };
            self.at += 1;
            let right = self.factor()?;
            left = binop(op, left, right)?;
        }
        Ok(left)
    }

    // factor: ('+' | '-') factor | power
    fn factor(&mut self) -> Result<Num, NotArithmetic> {
        match self.peek() {
            Some(Tok::Plus) => {
                self.at += 1;
                self.enter()?;
                let v = self.factor()?;
                self.depth -= 1;
                Ok(v)
            }
            Some(Tok::Minus) => {
                self.at += 1;
                self.enter()?;
                let v = self.factor()?;
                self.depth -= 1;
                Ok(match v {
                    Num::Int(i) => Num::Int(-i),
                    Num::Float(f) => Num::Float(-f),
                })
            }
            _ => self.power(),
        }
    }

    // power: primary ['**' factor]
    fn power(&mut self) -> Result<Num, NotArithmetic> {
        let base = self.primary()?;
        if self.peek() == Some(&Tok::DoubleStar) {
            self.at += 1;
            self.enter()?;
            let exponent = self.factor()?;
            self.depth -= 1;
            return binop(Op::Pow, base, exponent);
        }
        Ok(base)
    }

    // primary: NUMBER | '(' sum ')'; a call ("2(3)") or "()" is not arithmetic
    fn primary(&mut self) -> Result<Num, NotArithmetic> {
        let value = match self.toks.get(self.at).cloned() {
            Some(Tok::Num(n)) => {
                self.at += 1;
                n
            }
            Some(Tok::Open) => {
                self.at += 1;
                if self.peek() == Some(&Tok::Close) {
                    return Err(NotArithmetic); // ()
                }
                let v = self.sum()?;
                if self.peek() != Some(&Tok::Close) {
                    return Err(NotArithmetic);
                }
                self.at += 1;
                v
            }
            _ => return Err(NotArithmetic),
        };
        if self.peek() == Some(&Tok::Open) {
            return Err(NotArithmetic); // a call
        }
        Ok(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
}

/// `float(int)`, which raises OverflowError past the float range.
fn int_float(i: &BigInt) -> Result<f64, NotArithmetic> {
    i.to_f64().filter(|f| f.is_finite()).ok_or(NotArithmetic)
}

fn floats(a: &Num, b: &Num) -> Result<(f64, f64), NotArithmetic> {
    let f = |n: &Num| match n {
        Num::Int(i) => int_float(i),
        Num::Float(f) => Ok(*f),
    };
    Ok((f(a)?, f(b)?))
}

/// `abs(n) > limit`, int against float compared exactly as Python does.
fn abs_above(n: &Num, limit: i64) -> bool {
    match n {
        Num::Int(i) => i.abs() > BigInt::from(limit),
        Num::Float(f) => f.abs() > limit as f64,
    }
}

fn binop(op: Op, left: Num, right: Num) -> Result<Num, NotArithmetic> {
    if op == Op::Pow && (abs_above(&right, MAX_EXPONENT) || abs_above(&left, 1_000_000)) {
        return Err(NotArithmetic); // too big
    }
    if let (Num::Int(a), Num::Int(b)) = (&left, &right) {
        return match op {
            Op::Add => Ok(Num::Int(a + b)),
            Op::Sub => Ok(Num::Int(a - b)),
            Op::Mul => Ok(Num::Int(a * b)),
            Op::Div => int_true_divide(a, b).map(Num::Float),
            Op::Pow => match b.to_u32() {
                Some(e) => Ok(Num::Int(num_traits::Pow::pow(a, e))),
                // a negative exponent: int.__pow__ hands both to float_pow
                None => float_pow(int_float(a)?, int_float(b)?).map(Num::Float),
            },
        };
    }
    let (a, b) = floats(&left, &right)?;
    match op {
        Op::Add => Ok(Num::Float(a + b)),
        Op::Sub => Ok(Num::Float(a - b)),
        Op::Mul => Ok(Num::Float(a * b)),
        Op::Div => {
            if b == 0.0 {
                return Err(NotArithmetic); // ZeroDivisionError
            }
            Ok(Num::Float(a / b))
        }
        Op::Pow => float_pow(a, b).map(Num::Float),
    }
}

/// Python's `float.__pow__`: ZeroDivisionError for 0 to a negative power, a complex number (not arithmetic here)
/// for a negative base to a fractional power, OverflowError past the float range.
fn float_pow(a: f64, b: f64) -> Result<f64, NotArithmetic> {
    if b == 0.0 {
        return Ok(1.0);
    }
    if a == 0.0 && b < 0.0 && b.is_finite() {
        return Err(NotArithmetic);
    }
    if a < 0.0 && a.is_finite() && b.is_finite() && b != b.floor() {
        return Err(NotArithmetic);
    }
    let r = a.powf(b);
    if r.is_infinite() && a.is_finite() && b.is_finite() {
        return Err(NotArithmetic);
    }
    Ok(r)
}

/// `a / b` for two ints, correctly rounded as CPython's `long_true_divide` is.
fn int_true_divide(a: &BigInt, b: &BigInt) -> Result<f64, NotArithmetic> {
    if b.is_zero() {
        return Err(NotArithmetic);
    }
    if a.bits() <= 53 && b.bits() <= 53 {
        let (x, y) = (a.to_f64().ok_or(NotArithmetic)?, b.to_f64().ok_or(NotArithmetic)?);
        return Ok(x / y);
    }
    if a.is_zero() {
        return Ok(if a.is_negative() != b.is_negative() { -0.0 } else { 0.0 });
    }
    let negative = a.is_negative() != b.is_negative();
    let (n, d) = (a.abs(), b.abs());
    // a quotient of 55 or 56 bits: two bits past the 53 a float keeps, the lowest one sticky
    let shift = n.bits() as i64 - d.bits() as i64 - 55;
    let (q, r) = if shift >= 0 {
        let d = &d << (shift as u64);
        (&n / &d, &n % &d)
    } else {
        let n = &n << ((-shift) as u64);
        (&n / &d, &n % &d)
    };
    let mut q = q.to_u64().ok_or(NotArithmetic)?;
    if !r.is_zero() {
        q |= 1;
    }
    let mut value = q as f64;
    // times 2**shift in two steps, so that no step overflows or underflows on its own
    let half = (shift / 2) as i32;
    value *= 2f64.powi(half);
    value *= 2f64.powi(shift as i32 - half);
    if value.is_infinite() {
        return Err(NotArithmetic); // integer division result too large for a float
    }
    Ok(if negative { -value } else { value })
}

fn evaluate(src: &str) -> Result<Num, NotArithmetic> {
    let toks = tokenize(src)?;
    if toks.is_empty() {
        return Err(NotArithmetic);
    }
    let mut p = Parser { toks, at: 0, depth: 0 };
    let value = p.sum()?;
    if p.at != p.toks.len() {
        return Err(NotArithmetic);
    }
    Ok(value)
}

/// `f"{digits:,}"` for a run of digits.
fn commas(digits: &str) -> String {
    let bytes = digits.as_bytes();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

/// `f"{v:,}"` for an int.
fn int_commas(text: &str) -> String {
    match text.strip_prefix('-') {
        Some(rest) => format!("-{}", commas(rest)),
        None => commas(text),
    }
}

/// `f"{v:,.4f}".rstrip("0").rstrip(".")`.
fn four_places(v: f64) -> String {
    let s = format!("{v:.4}");
    let (sign, rest) = match s.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", s.as_str()),
    };
    let (int, frac) = rest.split_once('.').unwrap_or((rest, ""));
    let out = format!("{sign}{}.{frac}", commas(int));
    out.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Python's `_num`: a whole number with thousands commas, else up to four decimals.
pub fn num_text(v: &Num) -> String {
    match v {
        Num::Int(i) => {
            if i.abs() < BigInt::from(1_000_000_000_000_000i64) {
                int_commas(&i.to_string())
            } else {
                four_places(i.to_f64().unwrap_or(f64::INFINITY))
            }
        }
        Num::Float(f) => {
            let r = f.round_ties_even();
            if (f - r).abs() < 1e-9 && f.abs() < 1e15 {
                int_commas(&format!("{}", r as i64))
            } else {
                four_places(*f)
            }
        }
    }
}

// ---- conversions --------------------------------------------------------------------------------------------------

/// name -> (dimension, factor to the base unit). Base: metre, gram, millilitre, km/h.
pub static UNITS: LazyLock<HashMap<&'static str, (&'static str, f64)>> = LazyLock::new(|| {
    let mut units = HashMap::new();
    let mut unit = |dim: &'static str, factor: f64, names: &[&'static str]| {
        for n in names {
            units.insert(*n, (dim, factor));
        }
    };
    unit("length", 0.001, &["mm", "millimeter", "millimeters", "millimetre", "millimetres"]);
    unit("length", 0.01, &["cm", "centimeter", "centimeters", "centimetre", "centimetres"]);
    unit("length", 1.0, &["m", "meter", "meters", "metre", "metres"]);
    unit("length", 1000.0, &["km", "kilometer", "kilometers", "kilometre", "kilometres", "k"]);
    unit("length", 0.0254, &["in", "inch", "inches"]);
    unit("length", 0.3048, &["ft", "foot", "feet"]);
    unit("length", 0.9144, &["yd", "yard", "yards"]);
    unit("length", 1609.344, &["mi", "mile", "miles"]);
    unit("mass", 1.0, &["g", "gram", "grams"]);
    unit("mass", 1000.0, &["kg", "kilo", "kilos", "kilogram", "kilograms"]);
    unit("mass", 28.349523125, &["oz", "ounce", "ounces"]);
    unit("mass", 453.59237, &["lb", "lbs", "pound", "pounds"]);
    unit("mass", 6350.29318, &["stone", "stones"]);
    unit("volume", 1.0, &["ml", "milliliter", "milliliters", "millilitre", "millilitres"]);
    unit("volume", 1000.0, &["l", "liter", "liters", "litre", "litres"]);
    unit("volume", 4.92892159375, &["tsp", "teaspoon", "teaspoons"]);
    unit("volume", 14.78676478125, &["tbsp", "tablespoon", "tablespoons"]);
    unit("volume", 29.5735295625, &["fl oz", "fluid ounce", "fluid ounces"]);
    unit("volume", 236.5882365, &["cup", "cups"]);
    unit("volume", 473.176473, &["pint", "pints"]);
    unit("volume", 946.352946, &["quart", "quarts"]);
    unit("volume", 3785.411784, &["gallon", "gallons"]);
    unit("speed", 1.0, &["kph", "km/h", "kmh", "kilometers per hour", "kilometres per hour"]);
    unit("speed", 1.609344, &["mph", "miles per hour"]);
    units
});

static TEMPS: LazyLock<HashMap<&'static str, char>> = LazyLock::new(|| {
    HashMap::from([
        ("f", 'F'),
        ("fahrenheit", 'F'),
        ("degrees fahrenheit", 'F'),
        ("degrees f", 'F'),
        ("c", 'C'),
        ("celsius", 'C'),
        ("centigrade", 'C'),
        ("degrees celsius", 'C'),
        ("degrees c", 'C'),
    ])
});

fn unit_group(name: &str) -> String {
    format!("(?P<{name}>[a-z/ ]{{1,22}}?)")
}

static CONVERT_LEAD: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:convert|what(?:'s| is)|how much is|how many is)\s+"));
static CONVERT_TO: LazyLock<Regex> = LazyLock::new(|| {
    full(&format!(r"(?P<n>-?\d+(?:\.\d+)?) ?{} (?:to|in|into|in to) {}", unit_group("src"), unit_group("dst")))
});
static CONVERT_HOW_MANY: LazyLock<Regex> = LazyLock::new(|| {
    full(&format!(
        r"how many {} (?:are )?(?:in|is|are in) (?:a |an |one )?(?:(?P<n>-?\d+(?:\.\d+)?) ?)?{}",
        unit_group("dst"),
        unit_group("src")
    ))
});

/// Python's `_convert`: `value` in `src` as `dst`, or None when they are not the same kind of unit.
pub fn convert(value: f64, src: &str, dst: &str) -> Option<f64> {
    if let (Some(a), Some(b)) = (TEMPS.get(src), TEMPS.get(dst)) {
        if a == b {
            return Some(value);
        }
        return Some(if *a == 'C' { value * 9.0 / 5.0 + 32.0 } else { (value - 32.0) * 5.0 / 9.0 });
    }
    let (s, d) = (UNITS.get(src)?, UNITS.get(dst)?);
    if s.0 != d.0 {
        return None;
    }
    Some(value * s.1 / d.1)
}

/// The conversion for words that are only a unit conversion.
pub fn conversion(t: &str) -> Option<Conversion> {
    let body = CONVERT_LEAD.replace(t, "");
    let m = CONVERT_TO.captures(&body).or_else(|| CONVERT_HOW_MANY.captures(t))?;
    let src = py::strip(group(&m, "src")?).to_string();
    let dst = py::strip(group(&m, "dst")?).to_string();
    let value = match group(&m, "n") {
        Some(n) if !n.is_empty() => digits_float(n)?,
        _ => 1.0,
    };
    let result = convert(value, &src, &dst)?;
    Some(Conversion { value, src, dst, result })
}

// ---- countdowns ---------------------------------------------------------------------------------------------------

/// The `n`th `weekday` (Monday = 0) of a month.
pub fn nth_weekday(year: i32, month: u32, weekday: u32, n: u32) -> Option<NaiveDate> {
    let first = NaiveDate::from_ymd_opt(year, month, 1)?;
    let offset = (weekday + 7 - first.weekday().num_days_from_monday()) % 7 + 7 * (n - 1);
    first.checked_add_days(chrono::Days::new(u64::from(offset)))
}

type Holiday = fn(i32) -> Option<NaiveDate>;

pub static HOLIDAYS: LazyLock<HashMap<&'static str, Holiday>> = LazyLock::new(|| {
    let mut h: HashMap<&'static str, Holiday> = HashMap::new();
    h.insert("christmas", |y| NaiveDate::from_ymd_opt(y, 12, 25));
    h.insert("christmas eve", |y| NaiveDate::from_ymd_opt(y, 12, 24));
    h.insert("new year", |y| NaiveDate::from_ymd_opt(y, 1, 1));
    h.insert("new year's", |y| NaiveDate::from_ymd_opt(y, 1, 1));
    h.insert("new years", |y| NaiveDate::from_ymd_opt(y, 1, 1));
    h.insert("new year's day", |y| NaiveDate::from_ymd_opt(y, 1, 1));
    h.insert("new year's eve", |y| NaiveDate::from_ymd_opt(y, 12, 31));
    h.insert("halloween", |y| NaiveDate::from_ymd_opt(y, 10, 31));
    h.insert("valentine's day", |y| NaiveDate::from_ymd_opt(y, 2, 14));
    h.insert("valentines day", |y| NaiveDate::from_ymd_opt(y, 2, 14));
    h.insert("the fourth of july", |y| NaiveDate::from_ymd_opt(y, 7, 4));
    h.insert("fourth of july", |y| NaiveDate::from_ymd_opt(y, 7, 4));
    h.insert("july fourth", |y| NaiveDate::from_ymd_opt(y, 7, 4));
    h.insert("independence day", |y| NaiveDate::from_ymd_opt(y, 7, 4));
    h.insert("thanksgiving", |y| nth_weekday(y, 11, 3, 4)); // the fourth Thursday of November
    h
});

static NEXT: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:next )"));

/// (its name, its next date on or after today), for a holiday this module knows.
pub fn holiday(what: &str, today: NaiveDate) -> Option<(String, NaiveDate)> {
    let key = SPACES.replace_all(&py::strip(what).to_lowercase(), " ").into_owned();
    let key = NEXT.replace(&key, "").into_owned();
    let f = HOLIDAYS.get(key.as_str())?;
    let mut day = f(today.year())?;
    if day < today {
        day = f(today.year() + 1)?;
    }
    Some((key, day))
}

// ---- the words ----------------------------------------------------------------------------------------------------

pub fn time_line(now: &NaiveDateTime, place: &str) -> String {
    let clock = now.format("%-I:%M %p").to_string();
    if place.is_empty() {
        format!("It's {clock}.")
    } else {
        format!("It's {clock} in {place}.")
    }
}

pub fn date_line(now: &NaiveDateTime) -> String {
    format!("It's {} {}.", now.format("%A, %B"), now.day())
}

/// WMO weather interpretation codes (Open-Meteo's weather_code), in words.
pub fn wmo(code: i128) -> Option<&'static str> {
    Some(match code {
        0 => "clear",
        1 => "mostly clear",
        2 => "partly cloudy",
        3 => "overcast",
        45 | 48 => "foggy",
        51 => "light drizzle",
        53 => "drizzle",
        55 => "heavy drizzle",
        56 | 57 => "freezing drizzle",
        61 => "light rain",
        63 => "rain",
        65 => "heavy rain",
        66 | 67 => "freezing rain",
        71 => "light snow",
        73 => "snow",
        75 => "heavy snow",
        77 => "snow grains",
        80 => "light showers",
        81 => "showers",
        82 => "heavy showers",
        85 => "snow showers",
        86 => "heavy snow showers",
        95 => "thunderstorms",
        96 | 99 => "thunderstorms with hail",
        _ => return None,
    })
}

/// `x[i]` for a JSON value: a list's item, or a string's character; None where Python raises.
fn index(v: &Value, i: usize) -> Option<Value> {
    match v {
        Value::Array(a) => a.get(i).cloned(),
        Value::String(s) => s.chars().nth(i).map(|c| Value::String(c.to_string())),
        _ => None,
    }
}

/// Python's `float(s)` for a string: blanks around, `_` between digits, `inf`/`infinity`/`nan` in any case.
fn str_float(s: &str) -> Option<f64> {
    let t = py::strip(s);
    let chars: Vec<char> = t.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if *c == '_' {
            let ok = i > 0
                && chars[i - 1].is_ascii_digit()
                && chars.get(i + 1).is_some_and(char::is_ascii_digit);
            if !ok {
                return None;
            }
        }
    }
    let t: String = chars.into_iter().filter(|c| *c != '_').collect();
    let lower = t.to_ascii_lowercase();
    let unsigned = lower.trim_start_matches(['+', '-']);
    if lower.len() - unsigned.len() > 1 {
        return None;
    }
    let negative = lower.starts_with('-');
    let special = match unsigned {
        "inf" | "infinity" => Some(f64::INFINITY),
        "nan" => Some(f64::NAN),
        _ => None,
    };
    if let Some(v) = special {
        return Some(if negative { -v } else { v });
    }
    static FLOAT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[+-]?(?:\d+\.?\d*|\.\d+)(?:[eE][+-]?\d+)?$").expect("a pattern"));
    if !FLOAT.is_match(&t) {
        return None;
    }
    t.parse::<f64>().ok()
}

/// Python's `float(v)` for a JSON value; None where it raises.
fn py_float(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::String(s) => str_float(s),
        _ => None,
    }
}

/// Python's `int(v)` for a JSON value (a float is truncated); None where it raises.
fn py_int(v: &Value) -> Option<i128> {
    match v {
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(i128::from(i))
            } else if let Some(u) = n.as_u64() {
                Some(i128::from(u))
            } else {
                let f = n.as_f64()?;
                f.is_finite().then(|| f.trunc() as i128)
            }
        }
        Value::Bool(b) => Some(i128::from(*b)),
        Value::String(s) => {
            let t = py::strip(s);
            let digits = t.strip_prefix(['+', '-']).unwrap_or(t);
            let chars: Vec<char> = digits.chars().collect();
            if chars.is_empty() || !chars[0].is_ascii_digit() || !chars[chars.len() - 1].is_ascii_digit() {
                return None;
            }
            for (i, c) in chars.iter().enumerate() {
                let ok = c.is_ascii_digit() || (*c == '_' && chars[i - 1].is_ascii_digit() && chars[i + 1].is_ascii_digit());
                if !ok {
                    return None;
                }
            }
            let clean: String = t.chars().filter(|c| *c != '_').collect();
            clean.parse::<i128>().ok().or(Some(i128::MAX)) // too long for i128: no weather code either way
        }
        _ => None,
    }
}

/// `round(x)` for a float: ties to even; None for NaN and infinity (ValueError, OverflowError).
fn round_float(x: f64) -> Option<f64> {
    x.is_finite().then(|| x.round_ties_even() + 0.0)
}

/// An integral float as Python prints the int `round` made of it.
fn int_text(x: f64) -> String {
    format!("{x:.0}")
}

/// `round(rain)` for an int or float rain chance, as text; None when it is neither (no chance is given).
fn rain_text(rain: &Value) -> Result<Option<String>, ()> {
    match rain {
        Value::Bool(b) => Ok(Some(if *b { "1" } else { "0" }.into())),
        Value::Number(n) if n.is_i64() || n.is_u64() => Ok(Some(n.to_string())),
        Value::Number(n) => match n.as_f64().and_then(round_float) {
            Some(r) => Ok(Some(int_text(r))),
            None => Err(()),
        },
        _ => Ok(None),
    }
}

/// One spoken sentence from an Open-Meteo forecast; None when it lacks what the sentence needs.
pub fn weather_line(place: &str, data: &Value, day: usize) -> Option<String> {
    let daily = data.get("daily")?;
    if !daily.is_object() {
        return None;
    }
    let low = round_float(py_float(&index(daily.get("temperature_2m_min")?, day)?)?)?;
    let high = round_float(py_float(&index(daily.get("temperature_2m_max")?, day)?)?)?;
    let rain = match daily.get("precipitation_probability_max") {
        None => Value::Null,
        Some(v) => index(v, day)?,
    };
    let day_sky = match daily.get("weather_code") {
        Some(codes) => wmo(py_int(&index(codes, day)?)?).unwrap_or(""),
        None => "",
    };
    let mut now_part = None;
    if day == 0 {
        let cur = data.get("current")?;
        if !cur.is_object() {
            return None;
        }
        let temp = round_float(py_float(cur.get("temperature_2m")?)?)?;
        let feels = round_float(py_float(cur.get("apparent_temperature")?)?)?;
        let sky = wmo(py_int(cur.get("weather_code")?)?).unwrap_or("");
        let wind = round_float(py_float(cur.get("wind_speed_10m")?)?)?;
        now_part = Some((temp, feels, sky, wind));
    }
    let chance = match rain_text(&rain).ok()? {
        Some(r) => format!(", {r}% chance of rain"),
        None => String::new(),
    };
    let (low, high) = (int_text(low), int_text(high));
    if day == 1 {
        let sky = if day_sky.is_empty() { String::new() } else { format!("{day_sky}, ") };
        return Some(format!("Tomorrow in {place}: {sky}{low} to {high}°F{chance}."));
    }
    let (temp, feels, sky, wind) = now_part?; // a day past tomorrow has no "now" (Python: unbound)
    let mut now = format!("{}°F", int_text(temp));
    if !sky.is_empty() {
        now.push_str(&format!(" and {sky}"));
    }
    if (feels - temp).abs() >= 3.0 {
        now.push_str(&format!(", feels like {}°F", int_text(feels)));
    }
    Some(format!("In {place} it's {now}, wind {} mph. Today {low} to {high}°F{chance}.", int_text(wind)))
}

/// `str.capitalize()`.
fn capitalize(s: &str) -> String {
    let mut it = s.chars();
    match it.next() {
        Some(c) => c.to_uppercase().chain(it.flat_map(char::to_lowercase)).collect(),
        None => String::new(),
    }
}

pub fn sun_line(place: &str, data: &Value, which: &str, day: usize) -> Option<String> {
    let stamp = index(data.get("daily")?.get(which)?, day)?;
    let at = py::fromisoformat(stamp.as_str()?).ok()?;
    let when = if day == 1 { "tomorrow" } else { "today" };
    Some(format!("{} in {place} is at {} {when}.", capitalize(which), at.naive.format("%-I:%M %p")))
}

pub const AQI_BANDS: [(i64, &str); 6] = [
    (50, "good"),
    (100, "moderate"),
    (150, "unhealthy for sensitive groups"),
    (200, "unhealthy"),
    (300, "very unhealthy"),
    (10_000, "hazardous"),
];

pub fn air_line(place: &str, data: &Value) -> Option<String> {
    let aqi = round_float(py_float(data.get("current")?.get("us_aqi")?)?)?;
    let band = AQI_BANDS.iter().find(|(top, _)| aqi <= *top as f64)?.1;
    Some(format!("The air in {place} is {band}: US AQI {}.", int_text(aqi)))
}

pub fn math_line(said: &str, value: &Num) -> String {
    format!("{} is {}.", upper_first(said), num_text(value))
}

pub fn convert_line(c: &Conversion) -> String {
    format!("{} {} is {} {}.", num_text(&Num::Float(c.value)), c.src, num_text(&Num::Float(c.result)), c.dst)
}

pub fn countdown_line(name: &str, day: NaiveDate, today: NaiveDate) -> String {
    let n = (day - today).num_days();
    let pretty = upper_first(name);
    if n == 0 {
        return format!("{pretty} is today!");
    }
    let s = if n != 1 { "s" } else { "" };
    format!("{n} day{s} until {pretty}, on {} {}.", day.format("%A, %B"), day.day())
}

static PMSET_LINE: LazyLock<Regex> = LazyLock::new(|| re(r"(\d+)%;\s*([A-Za-z ]+?);"));

pub fn battery_line(pmset: &str) -> Option<String> {
    let m = PMSET_LINE.captures(pmset)?;
    let state = py::strip(&m[2]).to_lowercase();
    let plugged = pmset.contains("AC Power");
    let mut words = match state.as_str() {
        "charging" => "charging".to_string(),
        "charged" => "fully charged".to_string(),
        "discharging" => "on battery".to_string(),
        "finishing charge" => "finishing its charge".to_string(),
        "ac attached" => "plugged in, not charging".to_string(),
        other => other.to_string(),
    };
    if state == "discharging" && plugged {
        words = "plugged in".to_string();
    }
    Some(format!("The Mac is at {}%, {words}.", &m[1]))
}

// ---- the answers --------------------------------------------------------------------------------------------------

/// `repr(s)` for a string, as Python's `%r` writes it.
fn py_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `ZoneInfo(name)`, or None where Python raises: zoneinfo opens a file under the zone directory, so on the Mac's
/// case-insensitive file system a key in the wrong case still loads.
fn zone_named(name: &str) -> Option<Tz> {
    zone_info(name).ok()
}

/// `os.path.normpath` for a relative POSIX path.
fn normpath(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." if parts.last().is_some_and(|p| *p != "..") => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

/// `ZoneInfo(key)` with the error it raises: ValueError for a key that is absolute, not normalized or outside the
/// zone directory (zoneinfo checks that first), ZoneInfoNotFoundError for a key no zone has.
fn zone_info(name: &str) -> Result<Tz, Failed> {
    if name.starts_with('/') {
        return Err(Failed::ZoneKey);
    }
    let normal = normpath(name);
    if py::chars_len(&normal) != py::chars_len(name) || normal == ".." || normal.starts_with("../") {
        return Err(Failed::ZoneKey);
    }
    name.parse::<Tz>().ok().or_else(|| Tz::from_str_insensitive(name).ok()).ok_or(Failed::Zone)
}

fn env_get(environ: Option<&Env>, key: &str) -> Option<String> {
    match environ {
        Some(map) => map.get(key).cloned(),
        None => std::env::var(key).ok(),
    }
}

/// EVE_TIMEZONE as a zone, else None (the Mac's).
pub fn zone(environ: Option<&Env>) -> Option<Tz> {
    let raw = env_get(environ, "EVE_TIMEZONE").unwrap_or_default();
    let name = py::strip(&raw);
    if !name.is_empty() {
        match zone_named(name) {
            Some(tz) => return Some(tz),
            None => tracing::warn!("quick answers: EVE_TIMEZONE={} is not a time zone; using the Mac's", py_repr(name)),
        }
    }
    None
}

/// What a source call is lent: the URL and the query parameters, in order.
pub type Getter = Arc<dyn Fn(String, Map<String, Value>) -> BoxFuture<'static, anyhow::Result<Value>> + Send + Sync>;
/// The clock: now in the zone (None = the Mac's), as wall time.
pub type Clock = Arc<dyn Fn(Option<Tz>) -> NaiveDateTime + Send + Sync>;
/// `pmset -g batt`'s output.
pub type Pmset = Arc<dyn Fn() -> BoxFuture<'static, anyhow::Result<String>> + Send + Sync>;
/// Where the phone is now, or None; an error is Python's raise.
pub type Phone = Arc<dyn Fn() -> anyhow::Result<Option<Place>> + Send + Sync>;

/// A Getter from an async closure.
pub fn getter<F, Fut>(f: F) -> Getter
where
    F: Fn(String, Map<String, Value>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = anyhow::Result<Value>> + Send + 'static,
{
    Arc::new(move |url, params| Box::pin(f(url, params)))
}

/// A Pmset from an async closure.
pub fn pmset_fn<F, Fut>(f: F) -> Pmset
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = anyhow::Result<String>> + Send + 'static,
{
    Arc::new(move || Box::pin(f()))
}

/// The value as httpx writes it into a query string (`str` of a number, lower-case booleans).
fn query_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) if n.is_i64() || n.is_u64() => n.to_string(),
        Value::Number(n) => py::float_repr(n.as_f64().unwrap_or(f64::NAN)),
        Value::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// A source whose answer is not a 2xx (httpx's `raise_for_status`; redirects are not followed).
#[derive(Debug, thiserror::Error)]
#[error("HTTP status {0}")]
pub struct BadStatus(pub u16);

/// The client `get_json` uses: httpx's 5 s timeouts (connect and read), no redirects.
pub fn client() -> anyhow::Result<reqwest::Client> {
    let t = Duration::from_secs_f64(TIMEOUT_SECS);
    Ok(reqwest::Client::builder()
        .connect_timeout(t)
        .read_timeout(t)
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}

/// Python's `_get_json`: GET with the parameters, raise on a non-2xx, the JSON body.
pub async fn get_json(url: String, params: Map<String, Value>) -> anyhow::Result<Value> {
    get_json_with(&client()?, &url, &params).await
}

/// `_get_json` with the client lent (Python's `transport`).
pub async fn get_json_with(client: &reqwest::Client, url: &str, params: &Map<String, Value>) -> anyhow::Result<Value> {
    hide_coordinates(); // a test may have taken the filter off
    fetch_json(client, url, params).await
}

/// `urllib.parse.quote_plus(text, safe="")` as httpx writes a query: letters, digits and `_.-~` stay, a space is
/// `+`, every other byte of the UTF-8 is `%XX`.
fn quote_plus(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The query string httpx builds from the parameters, in their order.
fn query_string(params: &Map<String, Value>) -> String {
    params.iter().map(|(k, v)| format!("{}={}", quote_plus(k), quote_plus(&query_value(v)))).collect::<Vec<_>>().join("&")
}

/// The request itself, with httpx's request line in the log: `HTTP Request: GET <url> "HTTP/1.1 200 OK"`.
pub async fn fetch_json(client: &reqwest::Client, url: &str, params: &Map<String, Value>) -> anyhow::Result<Value> {
    let mut target = reqwest::Url::parse(url).map_err(|_| Failed::InvalidUrl)?;
    if !params.is_empty() {
        let query = query_string(params);
        let merged = match target.query() {
            Some(q) if !q.is_empty() => format!("{q}&{query}"),
            _ => query,
        };
        target.set_query(Some(&merged));
    }
    let resp = client.get(target).send().await?;
    let status = resp.status();
    log_http(&format!(
        "HTTP Request: GET {} \"{:?} {} {}\"",
        resp.url(),
        resp.version(),
        status.as_u16(),
        status.canonical_reason().unwrap_or("")
    ));
    if !status.is_success() {
        return Err(BadStatus(status.as_u16()).into());
    }
    Ok(resp.json::<Value>().await?)
}

/// `pmset -g batt`, within TIMEOUT_SECS.
pub async fn pmset() -> anyhow::Result<String> {
    let child = tokio::process::Command::new("pmset")
        .args(["-g", "batt"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let out = tokio::time::timeout(Duration::from_secs_f64(TIMEOUT_SECS), child.wait_with_output()).await??;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub tz: Option<String>,
}

/// The phone's fresh fix as "here" (phone_location::fresh), rounded to PHONE_DECIMALS; None without one.
pub fn phone_here(environ: Option<&Env>) -> Option<Place> {
    let fix = phone_location::fresh_within(environ, PHONE_FRESH_MIN as f64, None)?;
    Some(Place {
        name: fix.place.clone().filter(|p| !p.is_empty()).unwrap_or_else(|| PHONE_PLACE.to_string()),
        lat: py::round_to(fix.lat, PHONE_DECIMALS),
        lon: py::round_to(fix.lon, PHONE_DECIMALS),
        tz: None,
    })
}

/// Why an answer failed where Python raised; only its name reaches the log (never a URL, which could carry
/// coordinates).
#[derive(Debug, thiserror::Error)]
enum Failed {
    #[error("KeyError")]
    Key(&'static str),
    #[error("TypeError")]
    Shape(&'static str),
    #[error("ValueError")]
    Value(&'static str),
    #[error("ZoneInfoNotFoundError")]
    Zone,
    /// `ZoneInfo(key)` for a key that is not a normalized relative path.
    #[error("ValueError")]
    ZoneKey,
    /// `hit.get` on a hit that is not a dict.
    #[error("AttributeError")]
    Attribute(&'static str),
    #[error("InvalidURL")]
    InvalidUrl,
}

/// The kind of an error, for the log: the type, never its message.
fn error_name(e: &anyhow::Error) -> &'static str {
    if let Some(f) = e.downcast_ref::<Failed>() {
        return match f {
            Failed::Key(_) => "KeyError",
            Failed::Shape(_) => "TypeError",
            Failed::Value(_) => "ValueError",
            Failed::Zone => "ZoneInfoNotFoundError",
            Failed::ZoneKey => "ValueError",
            Failed::Attribute(_) => "AttributeError",
            Failed::InvalidUrl => "InvalidURL",
        };
    }
    if let Some(r) = e.downcast_ref::<reqwest::Error>() {
        // httpx raises the concrete class, and `type(e).__name__` names it
        return if r.is_timeout() && r.is_connect() {
            "ConnectTimeout"
        } else if r.is_timeout() {
            "ReadTimeout"
        } else if r.is_connect() {
            "ConnectError"
        } else if r.is_decode() {
            "JSONDecodeError"
        } else if r.is_body() {
            "ReadError"
        } else if r.is_request() {
            "RemoteProtocolError"
        } else {
            "HTTPError"
        };
    }
    if e.is::<BadStatus>() {
        return "HTTPStatusError";
    }
    if e.is::<tokio::time::error::Elapsed>() {
        return "TimeoutError";
    }
    if let Some(io) = e.downcast_ref::<std::io::Error>() {
        // the OSError subclass Python raises for the kind (a missing pmset is FileNotFoundError)
        return match io.kind() {
            std::io::ErrorKind::NotFound => "FileNotFoundError",
            std::io::ErrorKind::PermissionDenied => "PermissionError",
            std::io::ErrorKind::TimedOut => "TimeoutError",
            std::io::ErrorKind::BrokenPipe => "BrokenPipeError",
            std::io::ErrorKind::ConnectionRefused => "ConnectionRefusedError",
            std::io::ErrorKind::ConnectionReset => "ConnectionResetError",
            std::io::ErrorKind::ConnectionAborted => "ConnectionAbortedError",
            std::io::ErrorKind::Interrupted => "InterruptedError",
            _ => "OSError",
        };
    }
    "Error"
}

/// Python's truthiness for a JSON value.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `str(v)` for a JSON value.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        Value::Number(n) if n.is_i64() || n.is_u64() => n.to_string(),
        Value::Number(n) => py::float_repr(n.as_f64().unwrap_or(f64::NAN)),
        Value::Null => "None".to_string(),
        Value::Array(a) => format!("[{}]", a.iter().map(py_value_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter().map(|(k, v)| format!("{}: {}", py_repr(k), py_value_repr(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

fn py_value_repr(v: &Value) -> String {
    match v {
        Value::String(s) => py_repr(s),
        other => py_str(other),
    }
}

/// `float(v)` with the error Python raises: ValueError for a string that is not a number, TypeError for the rest.
fn float_of(v: &Value, what: &'static str) -> Result<f64, Failed> {
    py_float(v).ok_or(if v.is_string() { Failed::Value(what) } else { Failed::Shape(what) })
}

/// The geocoder's first hit as a Place: None for no hit; Err where Python raises.
fn geocoded(found: &Value, name: &str) -> anyhow::Result<Option<Place>> {
    let Value::Object(found) = found else {
        return Ok(None);
    };
    let hit = match found.get("results") {
        Some(results) if truthy(results) => match results {
            Value::Array(a) => a[0].clone(),
            Value::String(s) => Value::String(s.chars().next().map(String::from).unwrap_or_default()),
            Value::Object(_) => return Err(Failed::Key("results[0]").into()),
            _ => return Err(Failed::Shape("results[0]").into()),
        },
        _ => return Ok(None),
    };
    if !truthy(&hit) {
        return Ok(None);
    }
    let Value::Object(hit) = hit else {
        return Err(Failed::Attribute("hit.get").into());
    };
    let place_name = match hit.get("name") {
        Some(v) if truthy(v) => py_str(v),
        _ => name.to_string(),
    };
    let lat = float_of(hit.get("latitude").ok_or(Failed::Key("latitude"))?, "latitude")?;
    let lon = float_of(hit.get("longitude").ok_or(Failed::Key("longitude"))?, "longitude")?;
    // a timezone that is not a string cannot be a zone: the time there is the model's, as in Python
    let tz = hit.get("timezone").and_then(Value::as_str).map(str::to_string);
    Ok(Some(Place { name: place_name, lat, lon, tz }))
}

/// `match(text)` then `answer(ask)`: the words for a standard question, or None to let the model answer.
/// `phone`: where the phone is now, or None (phone_here, read on every question).
pub struct QuickAnswers {
    pub tz: Option<Tz>,
    get: Getter,
    now: Clock,
    pmset: Pmset,
    phone: Phone,
    pub home_name: String,
    home: Mutex<Option<Place>>,
    places: Mutex<HashMap<String, Option<Place>>>,
}

fn system_clock(tz: Option<Tz>) -> NaiveDateTime {
    match tz {
        Some(tz) => Utc::now().with_timezone(&tz).naive_local(),
        None => chrono::Local::now().naive_local(),
    }
}

impl QuickAnswers {
    /// From the environment lent (None: the process's), with the real sources: Open-Meteo, the clock, pmset and
    /// the phone's fix.
    pub fn new(environ: Option<Env>) -> Self {
        hide_coordinates();
        let env = environ.as_ref();
        let tz = zone(env);
        let home_name: String = py::split(&env_get(env, "EVE_WEATHER_PLACE").unwrap_or_default()).join(" ").chars().take(100).collect();
        let mut home = None;
        let lat = env_get(env, "EVE_WEATHER_LAT").unwrap_or_default();
        let lon = env_get(env, "EVE_WEATHER_LON").unwrap_or_default();
        if !lat.is_empty() && !lon.is_empty() {
            match (str_float(&lat), str_float(&lon)) {
                (Some(lat), Some(lon)) => {
                    let name = if home_name.is_empty() { "your area".to_string() } else { home_name.clone() };
                    home = Some(Place { name, lat, lon, tz: None });
                }
                _ => tracing::warn!("quick answers: EVE_WEATHER_LAT/LON are not numbers; ignored"),
            }
        }
        let phone_env = environ.clone();
        QuickAnswers {
            tz,
            get: getter(get_json),
            now: Arc::new(system_clock),
            pmset: pmset_fn(pmset),
            phone: Arc::new(move || Ok(phone_here(phone_env.as_ref()))),
            home_name,
            home: Mutex::new(home),
            places: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_get(mut self, get: Getter) -> Self {
        self.get = get;
        self
    }

    pub fn with_now(mut self, now: impl Fn(Option<Tz>) -> NaiveDateTime + Send + Sync + 'static) -> Self {
        self.now = Arc::new(now);
        self
    }

    pub fn with_pmset(mut self, pmset: Pmset) -> Self {
        self.pmset = pmset;
        self
    }

    pub fn with_phone(mut self, phone: impl Fn() -> anyhow::Result<Option<Place>> + Send + Sync + 'static) -> Self {
        self.phone = Arc::new(phone);
        self
    }

    /// The configured home, as it stands (looked up lazily from EVE_WEATHER_PLACE).
    pub fn home(&self) -> Option<Place> {
        self.home.lock().clone()
    }

    pub fn knows_here(&self) -> bool {
        self.home.lock().is_some() || !self.home_name.is_empty() || self.phone_place().is_some()
    }

    fn phone_place(&self) -> Option<Place> {
        match (self.phone)() {
            Ok(p) => p,
            Err(e) => {
                // no phone fix: the configured place, as before
                tracing::info!("quick answers: the phone's location unavailable ({})", error_name(&e));
                None
            }
        }
    }

    /// The question, or None when it is not a standard one or it is about "here" and here is not set.
    pub fn match_text(&self, text: &str) -> Option<Ask> {
        let ask = match_text(text)?;
        if matches!(ask.kind, Kind::Weather | Kind::Sun | Kind::Air) && ask.place.is_empty() && !self.knows_here() {
            return None; // "here" is not set: the model answers, as before
        }
        Some(ask)
    }

    /// The words, or None when a source failed (the model answers instead).
    pub async fn answer(&self, ask: &Ask) -> Option<String> {
        match self.answer_inner(ask).await {
            Ok(words) => words,
            Err(e) => {
                tracing::info!("quick answers: {} unavailable ({}); the model answers", ask.kind.as_str(), error_name(&e));
                None
            }
        }
    }

    async fn answer_inner(&self, ask: &Ask) -> anyhow::Result<Option<String>> {
        let now = (self.now)(self.tz);
        match ask.kind {
            Kind::Time => {
                if ask.place.is_empty() {
                    return Ok(Some(time_line(&now, "")));
                }
                let Some(place) = self.place(&ask.place).await? else {
                    return Ok(None);
                };
                let Some(tz) = place.tz.as_deref().filter(|t| !t.is_empty()) else {
                    return Ok(None);
                };
                let tz = zone_info(tz)?;
                return Ok(Some(time_line(&Utc::now().with_timezone(&tz).naive_local(), &place.name)));
            }
            Kind::Date => return Ok(Some(date_line(&now))),
            Kind::Math => {
                let Args::Math(sum) = &ask.args else {
                    return Err(Failed::Key("said").into());
                };
                return Ok(Some(math_line(&sum.said, &sum.value)));
            }
            Kind::Convert => {
                let Args::Convert(c) = &ask.args else {
                    return Err(Failed::Key("value").into());
                };
                return Ok(Some(convert_line(c)));
            }
            Kind::Countdown => {
                let Args::Countdown { what } = &ask.args else {
                    return Err(Failed::Key("what").into());
                };
                let today = now.date();
                return Ok(holiday(what, today).map(|(name, day)| countdown_line(&name, day, today)));
            }
            Kind::Battery => return Ok(battery_line(&(self.pmset)().await?)),
            Kind::Weather | Kind::Sun | Kind::Air => {}
        }
        let place = if ask.place.is_empty() { self.here().await? } else { self.place(&ask.place).await? };
        let Some(place) = place else {
            return Ok(None);
        };
        let mut params = Map::new();
        params.insert("latitude".into(), json!(place.lat));
        params.insert("longitude".into(), json!(place.lon));
        match ask.kind {
            Kind::Weather => {
                params.insert("current".into(), json!("temperature_2m,apparent_temperature,weather_code,wind_speed_10m"));
                params.insert("daily".into(), json!("temperature_2m_max,temperature_2m_min,precipitation_probability_max,weather_code"));
                params.insert("temperature_unit".into(), json!("fahrenheit"));
                params.insert("wind_speed_unit".into(), json!("mph"));
                params.insert("timezone".into(), json!("auto"));
                params.insert("forecast_days".into(), json!(2));
                let data = (self.get)(FORECAST_URL.to_string(), params).await?;
                Ok(if data.is_object() { weather_line(&place.name, &data, ask.day) } else { None })
            }
            Kind::Sun => {
                let Args::Sun { which } = &ask.args else {
                    return Err(Failed::Key("which").into());
                };
                params.insert("daily".into(), json!("sunrise,sunset"));
                params.insert("timezone".into(), json!("auto"));
                params.insert("forecast_days".into(), json!(2));
                let data = (self.get)(FORECAST_URL.to_string(), params).await?;
                Ok(if data.is_object() { sun_line(&place.name, &data, which, ask.day) } else { None })
            }
            _ => {
                params.insert("current".into(), json!("us_aqi"));
                params.insert("timezone".into(), json!("auto"));
                let data = (self.get)(AIR_URL.to_string(), params).await?;
                Ok(if data.is_object() { air_line(&place.name, &data) } else { None })
            }
        }
    }

    async fn here(&self) -> anyhow::Result<Option<Place>> {
        if let Some(phone) = self.phone_place() {
            return Ok(Some(phone)); // the phone's fresh fix beats the configured home
        }
        let missing = self.home.lock().is_none();
        if missing && !self.home_name.is_empty() {
            let found = self.place(&self.home_name).await?;
            *self.home.lock() = found;
        }
        Ok(self.home.lock().clone())
    }

    async fn place(&self, name: &str) -> anyhow::Result<Option<Place>> {
        let key = name.to_lowercase();
        if let Some(known) = self.places.lock().get(&key) {
            return Ok(known.clone());
        }
        let mut params = Map::new();
        params.insert("name".into(), json!(name));
        params.insert("count".into(), json!(1));
        let found = (self.get)(GEOCODE_URL.to_string(), params).await?;
        let place = geocoded(&found, name)?;
        self.places.lock().insert(key, place.clone());
        Ok(place)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(text: &str) -> Option<f64> {
        arithmetic(text).map(|s| s.value.to_f64())
    }

    #[test]
    fn python_precedence_and_associativity() {
        assert_eq!(n("-2 ** 2"), Some(-4.0));
        assert_eq!(n("2 ** -1"), None); // the operator check wants a digit after the operator, as in Python
        assert_eq!(n("1 * 2 ** -1"), Some(0.5));
        assert_eq!(n("2 ** 3 ** 2"), Some(512.0));
        assert_eq!(n("2 + 3 * 4"), Some(14.0));
        assert_eq!(n("10 - 4 - 3"), Some(3.0));
        assert_eq!(n("2 * -3"), None); // Python too: no digit right after the operator
        assert_eq!(n("2 - -3"), None);
        assert_eq!(n("2 -(-3)"), Some(5.0));
        assert_eq!(n("(2 + 3) * (4 - 1)"), Some(15.0));
    }

    #[test]
    fn what_python_refuses_is_refused() {
        for text in ["7 // 2", "2 (3)", "012 + 1", "1.2.3 + 1", "2 * * 3", "2 *** 3", "() + 1", "(-8) ** 0.5", "0 ** -1", "1 / 0.0"] {
            assert_eq!(arithmetic(text), None, "{text}");
        }
        assert_eq!(n("00 + 1"), Some(1.0));
        assert_eq!(n("007.5 + 1"), Some(8.5));
        assert_eq!(n(".5 + 5."), Some(5.5));
        // a float power past the range is OverflowError in Python, not infinity divided away
        assert_eq!(arithmetic("1 / 1000000.0 ** 60"), None);
    }

    #[test]
    fn ints_stay_exact() {
        let s = arithmetic("1000000 ** 3 + 1 - 1000000 ** 3").expect("a sum");
        assert_eq!(s.value, Num::Int(BigInt::from(1)));
        assert_eq!(num_text(&arithmetic("12345678 * 1000").expect("a sum").value), "12,345,678,000");
        assert_eq!(num_text(&arithmetic("1000000 ** 3").expect("a sum").value), "1,000,000,000,000,000,000");
        // a big int over a big int, correctly rounded
        assert_eq!(n("1000000 ** 50 / 1000000 ** 50"), Some(1.0));
        assert_eq!(n("1000000 ** 10 / 3"), Some(3.3333333333333335e59)); // Python: 10**60/3, not 1e60/3
        assert_eq!(n("7 / 2"), Some(3.5));
    }

    #[test]
    fn numbers_read_as_python_writes_them() {
        assert_eq!(num_text(&Num::Float(1234.5)), "1,234.5");
        assert_eq!(num_text(&Num::Float(-1234567.0)), "-1,234,567");
        assert_eq!(num_text(&Num::Float(3.106855961185994)), "3.1069");
        assert_eq!(num_text(&Num::Float(1e20)), "100,000,000,000,000,000,000");
        assert_eq!(num_text(&Num::Float(-0.00001)), "-0");
        assert_eq!(num_text(&Num::Float(2.5)), "2.5");
        assert_eq!(num_text(&Num::Float(1.03125)), "1.0312");
    }

    #[test]
    fn python_float_and_int_of_json() {
        assert_eq!(py_float(&json!(" 12.5 ")), Some(12.5));
        assert_eq!(py_float(&json!("1_000")), Some(1000.0));
        assert_eq!(py_float(&json!("1__0")), None);
        assert_eq!(py_float(&json!(true)), Some(1.0));
        assert_eq!(py_float(&Value::Null), None);
        assert!(py_float(&json!("-inf")).is_some_and(|f| f == f64::NEG_INFINITY));
        assert_eq!(py_int(&json!(2.9)), Some(2));
        assert_eq!(py_int(&json!(" 61 ")), Some(61));
        assert_eq!(py_int(&json!("2.0")), None);
    }

    #[test]
    fn the_python_whitespace_is_the_regex_whitespace() {
        assert_eq!(normalize("what\x1ctime is it"), "what time is it");
        assert_eq!(pyre(r"[^&\s]\s+"), r"[^&\s\x1c-\x1f][\s\x1c-\x1f]+");
    }

    #[test]
    fn the_query_is_written_as_httpx_writes_it() {
        let mut params = Map::new();
        params.insert("name".into(), json!("São Paulo ~*'!()/,:"));
        params.insert("count".into(), json!(1));
        params.insert("latitude".into(), json!(37.77));
        params.insert("b".into(), json!(true));
        // httpx 0.28: name=S%C3%A3o+Paulo+~%2A%27%21%28%29%2F%2C%3A&count=1&latitude=37.77&b=true
        assert_eq!(query_string(&params), "name=S%C3%A3o+Paulo+~%2A%27%21%28%29%2F%2C%3A&count=1&latitude=37.77&b=true");
    }

    #[test]
    fn a_zone_key_fails_as_zoneinfo_fails() {
        assert_eq!(zone_info("Asia/Tokyo").ok(), Some(chrono_tz::Asia::Tokyo));
        assert_eq!(zone_info("america/new_york").ok(), Some(chrono_tz::America::New_York));
        for key in ["", "/abs", "../etc", "..", "a//b", "./a", "a/", "a/../b"] {
            assert!(matches!(zone_info(key), Err(Failed::ZoneKey)), "{key:?}");
        }
        for key in ["Mars/Base", "America", "US/Pacific-New"] {
            assert!(matches!(zone_info(key), Err(Failed::Zone)), "{key:?}");
        }
    }

    #[test]
    fn the_error_names_are_pythons() {
        let name = |e: anyhow::Error| error_name(&e);
        assert_eq!(name(std::io::Error::from(std::io::ErrorKind::NotFound).into()), "FileNotFoundError");
        assert_eq!(name(std::io::Error::other("offline").into()), "OSError");
        assert_eq!(name(BadStatus(500).into()), "HTTPStatusError");
        assert_eq!(name(float_of(&json!("x"), "latitude").unwrap_err().into()), "ValueError");
        assert_eq!(name(float_of(&Value::Null, "latitude").unwrap_err().into()), "TypeError");
        assert_eq!(name(geocoded(&json!({"results": ["x"]}), "x").unwrap_err()), "AttributeError");
        assert_eq!(name(geocoded(&json!({"results": {"a": 1}}), "x").unwrap_err()), "KeyError");
        assert_eq!(name(geocoded(&json!({"results": [{"latitude": []}]}), "x").unwrap_err()), "TypeError");
    }

    #[test]
    fn any_decimal_digit_is_a_number_as_in_python() {
        // float("١٢.٥") == 12.5; float("𝟗") == 9.0 (the fourth run of ten in the mathematical digits)
        assert_eq!(digits_float("١٢.٥"), Some(12.5));
        assert_eq!(digits_float("-𝟗"), Some(-9.0));
        assert_eq!(digits_float("\u{1D7E1}"), Some(9.0));
        let s = arithmetic("١٥ percent of ٨٠").expect("a sum");
        assert_eq!((s.said.as_str(), s.value.to_f64()), ("15 percent of 80", 12.0));
        assert_eq!(conversion("٥ km to m").map(|c| c.result), Some(5000.0));
    }

    #[test]
    fn repr_quotes_as_python_does() {
        assert_eq!(py_repr("Mars/Base"), "'Mars/Base'");
        assert_eq!(py_repr("it's"), "\"it's\"");
    }
}
