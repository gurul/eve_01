//! Reading an Office document without a Mac task (eve lite).
//!
//! A .docx, .xlsx or .pptx is a zip of XML; an .rtf is text with control words. eve lite has no Mac task to open
//! one the way full eve does, so it pulls the words out here. The parse is hardened against a hostile file: a cap
//! on how many uncompressed bytes are read (a zip bomb), and a refusal of any part that declares a DTD or an entity
//! (OOXML never does, so this only rejects an attack — an external-entity fetch or a billion-laughs expansion).

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::sync::LazyLock;

use regex::bytes::Regex;
use roxmltree::{Document, Node};

use crate::{py, Upload, MAX_TEXT_CHARS};

/// Across all the parts read from one document.
pub const OFFICE_MAX_UNZIPPED: u64 = 64 * 1024 * 1024;
static XML_UNSAFE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<!(?:DOCTYPE|ENTITY)").expect("unsafe xml pattern"));
const WORD_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const DRAW_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const SHEET_NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";

/// One part reader: a member's XML text, or None (absent, over the budget, unreadable or unsafe).
type ReadPart<'a> = dyn FnMut(&str) -> Option<String> + 'a;
/// What pulls one format's words out through a part reader.
type Extract = fn(&mut ReadPart<'_>) -> String;

/// The words in a Word, Excel, PowerPoint or RTF upload, for eve lite's brain. "" when none come out. Errs only
/// when the file cannot be opened as what it claims (`brain_parts` logs it and reads it as empty).
pub fn office_text(upload: &Upload) -> anyhow::Result<String> {
    let suffix = py::path_suffix(&upload.path).to_lowercase();
    if suffix == ".rtf" || upload.mime == "application/rtf" {
        return Ok(rtf_text(&fs::read(&upload.path)?));
    }
    let extract: Extract = match suffix.as_str() {
        ".docx" => docx_from,
        ".pptx" => pptx_from,
        ".xlsx" => xlsx_from,
        _ => return Ok(String::new()),
    };
    zip_xml_text(&upload.path, extract)
}

/// The Python exception class a failure in `office_text` corresponds to, for the log line.
pub(crate) fn error_name(e: &anyhow::Error) -> &'static str {
    if e.downcast_ref::<zip::result::ZipError>().is_some() {
        "BadZipFile"
    } else if e.downcast_ref::<std::io::Error>().is_some() {
        "OSError"
    } else {
        "Exception"
    }
}

/// One OOXML part's text, or None when it is absent, not text, or declares a DTD or entity (never OOXML).
fn xml_text(data: &[u8]) -> Option<String> {
    if data.is_empty() || XML_UNSAFE.is_match(data) {
        return None;
    }
    match data {
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8(rest.to_vec()).ok(),
        [0xFF, 0xFE, rest @ ..] => utf16(rest, u16::from_le_bytes),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, u16::from_be_bytes),
        _ => String::from_utf8(data.to_vec()).ok(),
    }
}

fn utf16(data: &[u8], unit: fn([u8; 2]) -> u16) -> Option<String> {
    if !data.len().is_multiple_of(2) {
        return None;
    }
    let units: Vec<u16> = data.chunks_exact(2).map(|c| unit([c[0], c[1]])).collect();
    String::from_utf16(&units).ok()
}

/// Parse one part's text; None on a parse error (Python's `ET.ParseError`).
fn parse(text: &str) -> Option<Document<'_>> {
    Document::parse(text).ok()
}

/// Open the upload as a zip and hand `extract` a reader that returns one member's XML (bounded by
/// [`OFFICE_MAX_UNZIPPED`] across the call), so a zip bomb cannot be read whole.
fn zip_xml_text(path: &Path, extract: Extract) -> anyhow::Result<String> {
    let mut zf = zip::ZipArchive::new(fs::File::open(path)?)?;
    let mut sizes: HashMap<String, u64> = HashMap::new();
    for i in 0..zf.len() {
        let info = zf.by_index_raw(i)?;
        sizes.insert(info.name().to_string(), info.size());
    }
    let mut left = OFFICE_MAX_UNZIPPED;
    let mut read = |name: &str| -> Option<String> {
        let size = *sizes.get(name)?;
        if size > left {
            return None;
        }
        left -= size;
        let member = zf.by_name(name).ok()?;
        let mut data = Vec::new();
        member.take(size + 1).read_to_end(&mut data).ok()?;
        if data.len() as u64 > size {
            return None; // more than the directory declared: a damaged or hostile part
        }
        xml_text(&data)
    };
    Ok(py::strip(&extract(&mut read)).to_string())
}

fn is(node: &Node, ns: &str, name: &str) -> bool {
    node.is_element() && node.tag_name().name() == name && node.tag_name().namespace() == Some(ns)
}

/// `"".join(t.text for t in node.iter(tag) if t.text)`.
fn runs(node: Node, ns: &str, name: &str) -> Vec<String> {
    node.descendants()
        .filter(|n| is(n, ns, name))
        .filter_map(|n| n.text().filter(|t| !t.is_empty()).map(str::to_string))
        .collect()
}

/// A Word document's paragraphs: the <w:t> runs, joined, one line per <w:p>.
pub(crate) fn docx_from(read: &mut ReadPart<'_>) -> String {
    let Some(text) = read("word/document.xml") else { return String::new() };
    let Some(doc) = parse(&text) else { return String::new() };
    let lines: Vec<String> = doc
        .root_element()
        .descendants()
        .filter(|n| is(n, WORD_NS, "p"))
        .map(|para| runs(para, WORD_NS, "t").concat())
        .filter(|line| !py::strip(line).is_empty())
        .collect();
    lines.join("\n")
}

/// A PowerPoint deck's slides: the <a:t> runs of each slide in slide order, slides split by a blank line.
pub(crate) fn pptx_from(read: &mut ReadPart<'_>) -> String {
    let mut out: Vec<String> = Vec::new();
    for i in 1..=1000 {
        let Some(text) = read(&format!("ppt/slides/slide{i}.xml")) else { break };
        let Some(doc) = parse(&text) else { break };
        let slide = runs(doc.root_element(), DRAW_NS, "t");
        if !slide.is_empty() {
            out.push(slide.join(" "));
        }
    }
    out.join("\n\n")
}

/// The column letters of a cell reference ("B" from "B12"), or "" when there is none.
fn col(cell_ref: Option<&str>) -> String {
    cell_ref.unwrap_or("").chars().filter(|c| c.is_alphabetic()).collect()
}

/// An Excel book's sheets as tab-separated rows, each sheet under its number. Shared strings are resolved; a cell
/// keeps its column so a column can be read down (the finding's "total of column B").
pub(crate) fn xlsx_from(read: &mut ReadPart<'_>) -> String {
    let shared: Vec<String> = match read("xl/sharedStrings.xml") {
        Some(text) => match parse(&text) {
            Some(doc) => doc
                .root_element()
                .descendants()
                .filter(|n| is(n, SHEET_NS, "si"))
                .map(|si| runs(si, SHEET_NS, "t").concat())
                .collect(),
            None => Vec::new(),
        },
        None => Vec::new(),
    };
    let mut sheets: Vec<String> = Vec::new();
    for i in 1..=1000 {
        let Some(text) = read(&format!("xl/worksheets/sheet{i}.xml")) else { break };
        let Some(doc) = parse(&text) else { break };
        let mut rows: Vec<String> = Vec::new();
        for row in doc.root_element().descendants().filter(|n| is(n, SHEET_NS, "row")) {
            let mut cells: HashMap<String, String> = HashMap::new();
            for cell in row.descendants().filter(|n| is(n, SHEET_NS, "c")) {
                let value = cell_value(cell, &shared);
                if !value.is_empty() {
                    let mut key = col(cell.attribute("r"));
                    if key.is_empty() {
                        key = cells.len().to_string();
                    }
                    cells.insert(key, value);
                }
            }
            if !cells.is_empty() {
                let mut order: Vec<&String> = cells.keys().collect();
                order.sort_by(|a, b| a.chars().count().cmp(&b.chars().count()).then_with(|| a.cmp(b)));
                let line: Vec<String> = order.iter().map(|c| format!("{c}:{}", cells[*c])).collect();
                rows.push(line.join("\t"));
            }
        }
        if !rows.is_empty() {
            sheets.push(format!("Sheet {i}:\n{}", rows.join("\n")));
        }
    }
    sheets.join("\n\n")
}

/// One Excel cell's text: a shared or inline string, a formula's cached result, or a number, as it is stored.
fn cell_value(cell: Node, shared: &[String]) -> String {
    let kind = cell.attribute("t");
    if kind == Some("inlineStr") {
        return py::strip(&runs(cell, SHEET_NS, "t").concat()).to_string();
    }
    let text = cell
        .children()
        .find(|n| is(n, SHEET_NS, "v"))
        .map(|v| py::strip(v.text().unwrap_or("")).to_string())
        .unwrap_or_default();
    if kind == Some("s") {
        // shared[int(text)]: Python's int() and list indexing, a negative index counting from the end
        let Some(index) = py::parse_int(&text, 10) else { return String::new() };
        let len = shared.len() as i64;
        let at = if index < 0 { index + len } else { index };
        return if (0..len).contains(&at) { shared[at as usize].clone() } else { String::new() };
    }
    text
}

/// A control word at `i` (a backslash, letters, an optional signed number, an optional space) → (word, number,
/// end). Python's `\\([a-z]+)(-?\d+)? ?`, matched in place.
fn control_word(text: &[char], i: usize) -> Option<(String, Option<String>, usize)> {
    let n = text.len();
    if text.get(i) != Some(&'\\') {
        return None;
    }
    let mut j = i + 1;
    while j < n && text[j].is_ascii_lowercase() {
        j += 1;
    }
    if j == i + 1 {
        return None;
    }
    let word: String = text[i + 1..j].iter().collect();
    let mut number = None;
    let mut k = j;
    if k < n && text[k] == '-' {
        k += 1;
    }
    let digits_start = k;
    while k < n && text[k].is_ascii_digit() {
        k += 1;
    }
    if k > digits_start {
        number = Some(text[j..k].iter().collect::<String>());
        j = k;
    }
    if j < n && text[j] == ' ' {
        j += 1;
    }
    Some((word, number, j))
}

/// The readable text of an RTF file: control words and groups dropped, unicode escapes and \'xx bytes kept. A plain,
/// bounded reader — enough for eve lite to read a note, not a full RTF engine.
pub(crate) fn rtf_text(data: &[u8]) -> String {
    // latin-1: one character per byte
    let text: Vec<char> = data.iter().map(|b| char::from(*b)).collect();
    let n = text.len();
    let mut out: Vec<u32> = Vec::new();
    let (mut i, mut depth_skip) = (0usize, 0usize);
    while i < n && out.len() < MAX_TEXT_CHARS + 1 {
        let ch = text[i];
        if ch == '\\' {
            if i + 1 < n && matches!(text[i + 1], '\\' | '{' | '}') {
                out.push(text[i + 1] as u32);
                i += 2;
                continue;
            }
            if i + 1 < n && text[i + 1] == '\'' && i + 3 < n {
                // \'xx : one byte in the current code page
                let hex: String = text[i + 2..i + 4].iter().collect();
                if let Some(byte) = py::parse_int(&hex, 16).filter(|b| (0..=255).contains(b)) {
                    out.push(byte as u32);
                }
                i += 4;
                continue;
            }
            if let Some((word, number, end)) = control_word(&text, i) {
                // a control word, with an optional number
                if word == "u" && number.is_some() {
                    // \uN : a unicode character
                    out.push(mod_0x10000(number.as_deref().unwrap_or("")));
                } else if word == "par" || word == "line" {
                    out.push('\n' as u32);
                } else if matches!(word.as_str(), "fonttbl" | "colortbl" | "stylesheet" | "info" | "pict" | "object") {
                    depth_skip += 1; // a group to drop whole, at this brace depth
                }
                i = end;
                continue;
            }
            i += 1;
            continue;
        }
        if ch == '{' {
            i += 1;
            continue;
        }
        if ch == '}' {
            depth_skip = depth_skip.saturating_sub(1);
            i += 1;
            continue;
        }
        if depth_skip == 0 && ch != '\r' && ch != '\n' {
            out.push(ch as u32);
        }
        i += 1;
    }
    join_units(&out)
}

/// `int(number) % 0x10000` for `-?\d+` of any length (Python's ints do not overflow; the result is never negative).
fn mod_0x10000(number: &str) -> u32 {
    let (negative, digits) = match number.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, number),
    };
    let r = digits.bytes().fold(0u32, |r, d| (r * 10 + u32::from(d - b'0')) % 0x10000);
    if negative {
        (0x10000 - r) % 0x10000
    } else {
        r
    }
}

/// The pieces (code points, a surrogate half included: a Python string may hold one) as a string: a high and a low surrogate in a row are the one character they encode (as they are once
/// Python's JSON reaches the model); a lone surrogate, which a Rust string cannot hold, is U+FFFD.
fn join_units(units: &[u32]) -> String {
    let mut s = String::with_capacity(units.len());
    let mut k = 0;
    while k < units.len() {
        let u = units[k];
        if (0xD800..0xDC00).contains(&u) && k + 1 < units.len() && (0xDC00..0xE000).contains(&units[k + 1]) {
            let c = 0x10000 + ((u - 0xD800) << 10) + (units[k + 1] - 0xDC00);
            s.push(char::from_u32(c).unwrap_or('\u{FFFD}'));
            k += 2;
            continue;
        }
        s.push(char::from_u32(u).unwrap_or('\u{FFFD}'));
        k += 1;
    }
    s
}
