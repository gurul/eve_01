//! One shape for everything eve sends to Telegram (port of `telegram_format.py`).
//!
//! Before this module a message was whatever string reached `BotApi.send_message`: a model's
//! answer with its markdown intact (`**bold**` shown as stars, `## Heading` as hashes), a relayed
//! Claude Code message flattened onto one line, a task result as one long paragraph. Read on a phone,
//! that is a wall of text. Now every message is composed here, the same way:
//!
//! * a short **bold title** when the message is not eve's own voice (a task's result or question,
//!   Claude Code speaking through the relay, a permission to grant), one blank line, then the body;
//! * the body in short paragraphs: a blank line between paragraphs, `•` bullets, headings in bold,
//!   code in `<code>` / `<pre>`: markdown from a model turned into Telegram HTML, never shown raw;
//! * a markdown table as one block per row (Telegram has no tables, and a pipe grid in a proportional
//!   font is a wall of bars on a phone): the row's first cell in bold, then `header: value` lines;
//! * Telegram's HTML parse mode, which needs only `&`, `<` and `>` escaped in content (MarkdownV2
//!   needs eighteen characters escaped, and a missed one is a 400 and a lost message);
//! * split at 4096 characters on a paragraph boundary, then a line, then a space, with the open tags
//!   closed at the cut and reopened after it, so no piece is rejected by Telegram;
//! * no emoji, no em or en dashes (owner, 2026-09-21), stripped in code from every piece of prose.
//!
//! Pure: nothing here touches the network. [`compose`] builds one message, [`split`] cuts it,
//! [`visible`] is the plain text a piece shows (the fallback when Telegram will not parse it).
//!
//! Lengths and positions are counted in characters (Unicode code points), as Python's `len` and
//! slicing count them. The regexes keep Python's character classes: `\s` is `str.isspace` (which,
//! unlike Rust's `\s`, includes U+001C..U+001F), and `\w` and `\d` are read from CPython's
//! Unicode tables (`py_unicode.rs`).

mod html;
mod html5_entities;
mod py_unicode;

use std::sync::LazyLock;

use regex::{Captures, Regex};

/// Bot API limit on sendMessage text, HTML tags included.
pub const MAX_MESSAGE_CHARS: usize = 4096;
pub const PARSE_MODE: &str = "HTML";
pub const MAX_TITLE_CHARS: usize = 80;
pub const MAX_SUBTITLE_CHARS: usize = 100;
/// Room kept at a cut for the closing tags a balanced piece needs.
const TAG_HEADROOM: usize = 64;

/// Python's `str.isspace` set, as the inside of a regex character class (`\s` in a `str` pattern).
const WS: &str = r"\t\n\x0B\x0C\r\x1C-\x20\x{85}\x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}";
/// Python's `\w` and `\d` in a `str` pattern, from CPython's own Unicode tables (`py_unicode.rs`): the
/// `regex` crate's `\p{L}\p{N}` and `\d` follow a newer Unicode and would count letters Python does not.
use py_unicode::{DIGIT, WORD};

/// `str.isspace` for one character.
fn py_space(c: char) -> bool {
    matches!(c, '\t'..='\r' | '\x1c'..='\x20' | '\u{85}' | '\u{a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}'
        | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}')
}

fn py_strip(s: &str) -> &str {
    s.trim_matches(py_space)
}

fn py_rstrip(s: &str) -> &str {
    s.trim_end_matches(py_space)
}

/// Python's `len`: characters, not bytes.
fn clen(s: &str) -> usize {
    s.chars().count()
}

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid regex")
}

/// A pattern that needs look-around, which only a backtracking engine (like Python's `re`) runs.
/// Python's engine has no step limit, so neither does this one.
fn fancy(pattern: &str) -> fancy_regex::Regex {
    fancy_regex::RegexBuilder::new(pattern).backtrack_limit(usize::MAX).build().expect("valid regex")
}

/// `pattern.sub(repl, text)` for a look-around pattern.
type FCaps<'t> = fancy_regex::Captures<'t, str>;

fn fancy_sub(pattern: &fancy_regex::Regex, text: &str, repl: impl Fn(&FCaps) -> String) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for caps in pattern.captures_iter(text) {
        match caps {
            Ok(c) => {
                if let Some(m) = c.get(0) {
                    out.push_str(&text[last..m.start()]);
                    out.push_str(&repl(&c));
                    last = m.end();
                }
            }
            Err(e) => {
                tracing::warn!("telegram_format: regex gave up ({e}); the rest is left as it is");
                break;
            }
        }
    }
    out.push_str(&text[last..]);
    out
}

fn group<'t>(c: &FCaps<'t>, i: usize) -> &'t str {
    c.get(i).map_or("", |m| m.as_str())
}

// No emoji, ever (owner, 2026-09-21). Stripped in code from every outgoing message, prompt or not: the
// pictographic blocks, the variation selectors and joiners that build them, and the keycap combiner.
static EMOJI: LazyLock<Regex> = LazyLock::new(|| {
    re(r"[\x{1F000}-\x{1FAFF}\x{2600}-\x{27BF}\x{2B00}-\x{2BFF}\x{1F900}-\x{1F9FF}\x{FE0F}\x{200D}\x{20E3}\x{1F1E6}-\x{1F1FF}\x{231A}-\x{231B}\x{23E9}-\x{23FA}\x{25AA}-\x{25FE}\x{2934}-\x{2935}\x{3030}\x{303D}\x{3297}\x{3299}]")
});

// em dash, en dash (owner, 2026-09-21): a comma or a full stop instead
static DASH: LazyLock<Regex> = LazyLock::new(|| re(&format!("[{WS}]*[\u{2014}\u{2013}][{WS}]*")));
// a dash right before punctuation just goes (Python's `$`: the end, or just before a final newline)
static DASH_BEFORE_STOP: LazyLock<fancy_regex::Regex> = LazyLock::new(|| fancy(r", (?=[,.!?]|\n?\z)"));
static SPACE_RUN: LazyLock<Regex> = LazyLock::new(|| re(r"[ \t]{2,}"));

/// The text without emoji or dashes, and without the doubled spaces they leave behind. A dash between
/// words becomes a comma; one that ends a sentence-like run becomes a full stop.
pub fn plain(text: &str) -> String {
    let out = EMOJI.replace_all(text, "");
    let out = DASH.replace_all(&out, ", ");
    let out = fancy_sub(&DASH_BEFORE_STOP, &out, |_| String::new());
    if out == text {
        return text.to_string();
    }
    py_strip(&SPACE_RUN.replace_all(&out, " ")).to_string()
}

/// The three characters Telegram's HTML mode reserves. Everything else travels as it is.
pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

// ---- markdown-ish text → Telegram HTML ---------------------------------------------------------

static FENCE: LazyLock<Regex> = LazyLock::new(|| re(&format!("^[{WS}]*(```|~~~)")));
static HEADING: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"^#{{1,6}}[{WS}]+(.*?)[{WS}]*#*$")));
static NUMBERED: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"^([{DIGIT}]{{1,3}})[.)][{WS}]+(.*)$")));
static BULLET: LazyLock<Regex> = LazyLock::new(|| re(&format!("^(?:[-*+\u{2022}\u{25e6}])[{WS}]+(.*)$")));
static RULE: LazyLock<Regex> = LazyLock::new(|| re(&format!("^(?:[-*_][{WS}]*){{3,}}$")));
static QUOTE: LazyLock<Regex> = LazyLock::new(|| re(&format!("^>[{WS}]?(.*)$")));
static CODE_SPAN: LazyLock<Regex> = LazyLock::new(|| re("(`[^`\n]+`)"));
static LINK: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"\[([^\]\n]+)\]\((https?://[^){WS}]+)\)")));
static BOLD: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    fancy(&format!(r"\*\*(?=[^{WS}])(.+?)(?<=[^{WS}])\*\*|__(?=[^{WS}])(.+?)(?<=[^{WS}])__"))
});
static ITALIC_STAR: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    fancy(&format!(r"(?<![{WORD}*])\*(?=[^{WS}])([^*\n]+?)(?<=[^{WS}])\*(?![{WORD}*])"))
});
static ITALIC_UNDER: LazyLock<fancy_regex::Regex> =
    LazyLock::new(|| fancy(&format!(r"(?<![{WORD}])_(?=[^{WS}])([^_\n]+?)(?<=[^{WS}])_(?![{WORD}])")));
static STRIKE: LazyLock<fancy_regex::Regex> =
    LazyLock::new(|| fancy(&format!(r"~~(?=[^{WS}])(.+?)(?<=[^{WS}])~~")));
// |---|:---:|, the line under a header
static TABLE_SEP: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(r"^\|?[{WS}]*:?-+:?[{WS}]*(?:\|[{WS}]*:?-+:?[{WS}]*)*\|?$"))
});
static HELD_URL: LazyLock<Regex> = LazyLock::new(|| re(r"\x00(\d+)\x00"));

fn bold_inner(c: &FCaps) -> String {
    let one = group(c, 1);
    if one.is_empty() { group(c, 2) } else { one }.to_string()
}

/// Inline markdown on already-escaped prose: links, bold, italic, strikethrough. A link's URL is held out
/// while the styles run and put back after: run over it, "_foo_" in a path became <i> inside the href.
fn styles(escaped: &str) -> String {
    let mut urls: Vec<String> = Vec::new();
    // a NUL of the text's own is no placeholder
    let out = LINK
        .replace_all(&escaped.replace('\x00', ""), |c: &Captures| {
            urls.push(c[2].to_string());
            format!("<a href=\"\x00{}\x00\">{}</a>", urls.len() - 1, &c[1])
        })
        .into_owned();
    let out = fancy_sub(&BOLD, &out, |c| format!("<b>{}</b>", bold_inner(c)));
    let out = fancy_sub(&STRIKE, &out, |c| format!("<s>{}</s>", group(c, 1)));
    let out = fancy_sub(&ITALIC_STAR, &out, |c| format!("<i>{}</i>", group(c, 1)));
    let out = fancy_sub(&ITALIC_UNDER, &out, |c| format!("<i>{}</i>", group(c, 1)));
    if urls.is_empty() {
        return out;
    }
    HELD_URL
        .replace_all(&out, |c: &Captures| {
            c[1].parse::<usize>().ok().and_then(|i| urls.get(i)).cloned().unwrap_or_else(|| c[0].to_string())
        })
        .into_owned()
}

/// One line of prose as HTML: escaped, with `code` spans kept verbatim and the styles applied
/// around them.
pub fn inline(text: &str) -> String {
    let escaped = escape(text);
    // `_CODE_SPAN.split(...)`: the text between spans and the spans themselves, in order
    let mut parts: Vec<&str> = Vec::new();
    let mut last = 0;
    for m in CODE_SPAN.find_iter(&escaped) {
        parts.push(&escaped[last..m.start()]);
        parts.push(m.as_str());
        last = m.end();
    }
    parts.push(&escaped[last..]);
    let mut pieces = String::new();
    for part in parts {
        if clen(part) >= 3 && part.starts_with('`') && part.ends_with('`') {
            pieces.push_str("<code>");
            pieces.push_str(&part[1..part.len() - 1]);
            pieces.push_str("</code>");
        } else if !part.is_empty() {
            pieces.push_str(&styles(part));
        }
    }
    pieces
}

/// The cells of one `| a | b |` line, outer pipes dropped, each cell stripped.
fn cells(row: &str) -> Vec<String> {
    let body = py_strip(row);
    let body = body.strip_prefix('|').unwrap_or(body);
    let body = body.strip_suffix('|').unwrap_or(body);
    body.split('|').map(|c| py_strip(c).to_string()).collect()
}

/// A row's first cell, bold. Its own `**stars**` come off first: bold inside bold is two tags.
fn lead(cell: &str) -> String {
    format!("<b>{}</b>", inline(&fancy_sub(&BOLD, cell, bold_inner)))
}

/// A markdown table (header, separator, rows) as HTML paragraphs a phone can read. Telegram has no
/// tables, and a pipe grid in a proportional font lines nothing up. Two columns or fewer: one
/// paragraph, a line per row, `<b>first cell</b>: second`. Wider: a paragraph per row, the first
/// cell in bold, then a `header: value` line for every filled cell after it (an empty cell is no
/// line). Cells are prose: emoji and dashes go, inline styles and links stay.
pub fn table_blocks<S: AsRef<str>>(lines: &[S]) -> Vec<String> {
    let Some(first) = lines.first() else {
        return Vec::new();
    };
    let mut header: Vec<String> = cells(first.as_ref()).iter().map(|c| plain(c)).collect();
    let rows: Vec<Vec<String>> = lines
        .get(2..)
        .unwrap_or(&[])
        .iter()
        .map(|line| cells(line.as_ref()).iter().map(|c| plain(c)).collect::<Vec<String>>())
        .filter(|r| r.iter().any(|c| !c.is_empty()))
        .collect();
    let width = rows.iter().map(Vec::len).chain([header.len()]).max().unwrap_or(0);
    header.resize(width, String::new());
    if width <= 2 {
        let mut out: Vec<String> = Vec::new();
        for mut r in rows {
            r.resize(width, String::new());
            let first = if r[0].is_empty() { String::new() } else { lead(&r[0]) };
            let second = if width == 2 && !r[1].is_empty() { inline(&r[1]) } else { String::new() };
            out.push(if !first.is_empty() && !second.is_empty() {
                format!("{first}: {second}")
            } else if !first.is_empty() {
                first
            } else {
                second
            });
        }
        return if out.is_empty() { Vec::new() } else { vec![out.join("\n")] };
    }
    let mut blocks: Vec<String> = Vec::new();
    for mut r in rows {
        r.resize(width, String::new());
        let mut block: Vec<String> = if r[0].is_empty() { Vec::new() } else { vec![lead(&r[0])] };
        for (h, v) in header[1..].iter().zip(&r[1..]) {
            if v.is_empty() {
                continue;
            }
            let label = if h.is_empty() { String::new() } else { inline(h) + ": " };
            block.push(label + &inline(v));
        }
        if !block.is_empty() {
            blocks.push(block.join("\n"));
        }
    }
    blocks
}

/// A finished fence as a `<pre>` paragraph, when it holds anything.
fn push_code(paragraphs: &mut Vec<String>, fence: &[String]) {
    let joined = fence.join("\n");
    let code = joined.trim_matches('\n');
    if !py_strip(code).is_empty() {
        paragraphs.push(format!("<pre>{}</pre>", escape(&EMOJI.replace_all(code, ""))));
    }
}

/// Free text (a model's answer, a task's result, what Claude Code said) as Telegram HTML paragraphs.
///
/// Line structure is kept: a blank line separates paragraphs, and lines inside a paragraph stay on their
/// own lines. Fenced code becomes `<pre>`; `#` headings become bold lines; `-`, `*` and numbered
/// lists become `•` and `1.` lines; `> quotes` become italic; a horizontal rule goes; a `|` table
/// becomes [`table_blocks`]. Emoji and dashes are stripped from prose ([`plain`]), never from code.
pub fn render_body(text: &str) -> String {
    let mut paragraphs: Vec<String> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut fence: Option<Vec<String>> = None;

    fn flush(paragraphs: &mut Vec<String>, current: &mut Vec<String>) {
        if !current.is_empty() {
            paragraphs.push(current.join("\n"));
            current.clear();
        }
    }

    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normalized.split('\n').collect();
    let mut i = 0;
    while i < lines.len() {
        let raw = lines[i];
        i += 1;
        if FENCE.is_match(raw) {
            match fence.take() {
                None => {
                    flush(&mut paragraphs, &mut current);
                    fence = Some(Vec::new());
                }
                Some(code) => push_code(&mut paragraphs, &code),
            }
            continue;
        }
        if let Some(code) = fence.as_mut() {
            code.push(py_rstrip(raw).to_string());
            continue;
        }
        if py_strip(raw).starts_with('|') && i < lines.len() && TABLE_SEP.is_match(py_strip(lines[i])) {
            flush(&mut paragraphs, &mut current); // a header row over its separator: a table until a line
            let mut end = i + 1; // that does not start with a pipe
            while end < lines.len() && py_strip(lines[end]).starts_with('|') {
                end += 1;
            }
            paragraphs.extend(table_blocks(&lines[i - 1..end]));
            i = end;
            continue;
        }
        let line = plain(py_strip(raw));
        if line.is_empty() {
            flush(&mut paragraphs, &mut current);
            continue;
        }
        if RULE.is_match(&line) {
            flush(&mut paragraphs, &mut current);
            continue;
        }
        if let Some(m) = HEADING.captures(&line) {
            current.push(format!("<b>{}</b>", inline(&m[1])));
        } else if let Some(m) = NUMBERED.captures(&line) {
            current.push(format!("{}. {}", &m[1], inline(&m[2])));
        } else if let Some(m) = BULLET.captures(&line) {
            current.push(format!("\u{2022} {}", inline(&m[1])));
        } else if let Some(m) = QUOTE.captures(&line) {
            let quoted = py_strip(&m[1]);
            if !quoted.is_empty() {
                current.push(format!("<i>{}</i>", inline(quoted)));
            }
        } else {
            current.push(inline(&line));
        }
    }
    if let Some(code) = fence {
        // an unclosed fence: still code, still shown
        push_code(&mut paragraphs, &code);
    }
    flush(&mut paragraphs, &mut current);
    paragraphs.join("\n\n")
}

fn clip(text: &str, limit: usize) -> String {
    let collapsed: Vec<&str> = text.split(py_space).filter(|w| !w.is_empty()).collect();
    let text = plain(&collapsed.join(" "));
    if clen(&text) <= limit {
        return text;
    }
    let head: String = text.chars().take(limit.saturating_sub(1)).collect();
    format!("{}\u{2026}", py_rstrip(&head))
}

/// One message: a bold title (when the message is not eve's own voice), an italic subtitle under it
/// when an identifier helps (the task's goal, the repo Claude speaks from), a blank line, the body.
pub fn compose(body: &str, title: Option<&str>, subtitle: Option<&str>) -> String {
    let mut head: Vec<String> = Vec::new();
    if let Some(t) = title.filter(|t| !py_strip(t).is_empty()) {
        head.push(format!("<b>{}</b>", escape(&clip(t, MAX_TITLE_CHARS))));
    }
    if let Some(s) = subtitle.filter(|s| !py_strip(s).is_empty()) {
        head.push(format!("<i>{}</i>", escape(&clip(s, MAX_SUBTITLE_CHARS))));
    }
    let mut parts: Vec<String> = Vec::new();
    if !head.is_empty() {
        parts.push(head.join("\n"));
    }
    let rendered = render_body(body);
    if !rendered.is_empty() {
        parts.push(rendered);
    }
    parts.join("\n\n")
}

// ---- splitting at the Bot API limit, tags kept balanced ------------------------------------------

static TAG: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"<(/?)(b|i|s|u|code|pre|a)([{WS}][^<>]*)?>")));

/// The tags still open at the end of `piece`, outermost first, with their attributes.
fn open_tags(piece: &str) -> Vec<(String, String)> {
    let mut stack: Vec<(String, String)> = Vec::new();
    for m in TAG.captures_iter(piece) {
        let name = &m[2];
        if &m[1] == "/" {
            if stack.last().is_some_and(|(top, _)| top == name) {
                stack.pop();
            }
        } else {
            stack.push((name.to_string(), m.get(3).map_or("", |a| a.as_str()).to_string()));
        }
    }
    stack
}

/// `s.rfind(c, start, end)` over characters.
fn rfind(s: &[char], c: char, start: usize, end: usize) -> Option<usize> {
    let end = end.min(s.len());
    if start >= end {
        return None;
    }
    s[start..end].iter().rposition(|&x| x == c).map(|p| p + start)
}

/// `s.find(c, start, end)` over characters.
fn find(s: &[char], c: char, start: usize, end: usize) -> Option<usize> {
    let end = end.min(s.len());
    if start >= end {
        return None;
    }
    s[start..end].iter().position(|&x| x == c).map(|p| p + start)
}

/// Move a cut position left, out of the middle of a tag or an entity.
fn safe_cut(text: &[char], mut at: usize) -> usize {
    if let Some(lt) = rfind(text, '<', 0, at) {
        if find(text, '>', lt, at).is_none() {
            at = lt;
        }
    }
    if let Some(amp) = rfind(text, '&', at.saturating_sub(10), at) {
        if find(text, ';', amp, at).is_none() {
            at = amp;
        }
    }
    at
}

/// One paragraph longer than `limit`: cut at a line, then a space, then anywhere, with the open
/// tags closed at the cut and reopened after it.
fn slice(paragraph: &str, limit: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest: Vec<char> = paragraph.chars().collect();
    // At a limit of 0 or 1 Python's window is 0 and the loop never ends; one character is the least cut.
    let window = limit.saturating_sub(TAG_HEADROOM).max(limit / 2).max(1);
    let half = window / 2;
    while rest.len() > limit {
        // a line end in the second half of the window first, then a space there, else the window as it is
        let mut found = rfind(&rest, '\n', 0, window).filter(|&c| c >= half);
        if found.is_none() {
            found = rfind(&rest, ' ', 0, window).filter(|&c| c >= half);
        }
        let mut cut = found.map_or(window, |c| c + 1);
        cut = safe_cut(&rest, cut);
        if cut == 0 {
            cut = window;
        }
        let piece: String = rest[..cut].iter().collect();
        let tail: String = rest[cut..].iter().collect();
        let open = open_tags(&piece);
        let mut closed = py_rstrip(&piece).to_string();
        for (name, _) in open.iter().rev() {
            closed.push_str(&format!("</{name}>"));
        }
        let mut reopened: String = open.iter().map(|(name, attrs)| format!("<{name}{attrs}>")).collect();
        reopened.push_str(tail.trim_start_matches(' '));
        let next: Vec<char> = reopened.chars().collect();
        out.push(closed);
        // The reopened tags are at most the piece they came from, so a cut either shortens the rest or, when
        // the piece was nothing but those tags (a tiny limit under nested tags), gives the same rest back.
        // Python loops forever there; the rest goes out as it is.
        if next == rest {
            tracing::warn!("telegram_format: a paragraph cannot be cut to {limit} characters; sent as it is");
            break;
        }
        rest = next;
    }
    if !rest.is_empty() {
        out.push(rest.into_iter().collect());
    }
    out
}

/// The pieces to send, each at most `limit` characters ([`MAX_MESSAGE_CHARS`] for Telegram), cut on a
/// paragraph boundary when one is there. An empty message is no pieces.
pub fn split(text: &str, limit: usize) -> Vec<String> {
    if py_strip(text).is_empty() {
        return Vec::new();
    }
    if clen(text) <= limit {
        return vec![text.to_string()];
    }
    let mut pieces: Vec<String> = Vec::new();
    let mut current = String::new();
    for paragraph in text.split("\n\n") {
        if py_strip(paragraph).is_empty() {
            continue;
        }
        if clen(paragraph) > limit {
            // Its slices go out one per piece; the first may still follow what is waiting (a title), and
            // the last may still be joined by the paragraph after it.
            let slices = slice(paragraph, limit);
            let joined = if current.is_empty() { slices[0].clone() } else { format!("{current}\n\n{}", slices[0]) };
            if clen(&joined) <= limit {
                pieces.push(joined);
            } else {
                for p in [&current, &slices[0]] {
                    if !p.is_empty() {
                        pieces.push(p.clone());
                    }
                }
            }
            if slices.len() > 2 {
                pieces.extend(slices[1..slices.len() - 1].iter().cloned());
            }
            current = slices[slices.len() - 1].clone();
            continue;
        }
        let joined = if current.is_empty() { paragraph.to_string() } else { format!("{current}\n\n{paragraph}") };
        if clen(&joined) <= limit {
            current = joined;
        } else {
            pieces.push(std::mem::replace(&mut current, paragraph.to_string()));
        }
    }
    if !current.is_empty() {
        pieces.push(current);
    }
    pieces
}

/// The text a piece shows once Telegram has rendered it: tags gone, entities back to characters.
/// What is sent, without a parse mode, when Telegram refuses the HTML.
pub fn visible(piece: &str) -> String {
    html::unescape(&TAG.replace_all(piece, ""))
}
