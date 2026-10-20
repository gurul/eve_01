//! A port of `apps_maker.py`, lines 1127-1956: the model call, the edit/repair prompts, AppMaker, EVE_JS, ChatMaker.

use std::time::Instant;

use futures::{FutureExt, StreamExt};

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::apps_maker::*;

/// py: apps_maker.py:1130 `class Made:`
#[derive(Debug, Clone, Default)]
pub struct Made {
    pub ok: bool,
    pub app: Option<AppInfo>,
    pub reason: String,
    pub usd: f64,
    pub secs: f64,
    /// the check of the version that was kept
    pub check: Option<eve_app_check::CheckReport>,
    /// repair rounds used
    pub rounds: i64,
    /// what is still wrong with the kept version, if anything
    pub issues: Vec<String>,
    /// the problems each round found, in order
    pub history: Vec<Vec<String>>,
    /// What the build raised, where Python's `make` / `edit` raised it (Claude could not be reached before any
    /// version was written): `ok` is false and `reason` is `stopped_reason(error)`. A caller that turns the
    /// failure into a line (the chat door, the Mini App) reads this first, as Python's `except` did.
    pub error: Option<Arc<anyhow::Error>>,
}

impl Made {
    /// `Made(False, reason=...)`.
    pub fn failed(reason: impl Into<String>) -> Self {
        Self { ok: false, reason: reason.into(), ..Self::default() }
    }
}

/// py: apps_maker.py:1143 `class Generated:`
#[derive(Debug, Clone)]
pub struct Generated {
    /// the answer's text
    pub text: String,
    /// the API's usage, for the bill (Null: none)
    pub usage: Value,
    pub stop_reason: String,
    /// the assistant's final content, appended unchanged for a repair round (Null: none)
    pub content: Value,
}

// py: apps_maker.py:1152 `Progress`, 1153 `Generate`, 1154 `Check`: the type aliases MakerProgress, Generate and
// Check at the end of this file. progress(stage, n): stage is "thinking", "writing" (n = characters written so
// far), "testing", "fixing" (n = how many problems are being fixed) or "saving".

/// py: apps_maker.py:1157 `def _no_progress(stage: str, n: int) -> None:`
pub(crate) fn no_progress(stage: &str, n: i64) {
    let _ = (stage, n);
}

/// `_no_progress` as a [`MakerProgress`].
fn quiet() -> MakerProgress {
    Arc::new(no_progress)
}

/// `app_check.check_app`, the default `check` of an AppMaker.
pub fn real_check() -> Check {
    Arc::new(|html: String, opts: eve_app_check::CheckOptions| {
        async move { eve_app_check::check_app(&html, opts).await }.boxed()
    })
}

/// The body of one maker turn, as the SDK's `messages.stream(...)` sends it.
fn maker_body(model: &str, effort: &str, messages: Vec<Value>) -> Value {
    json!({
        "model": model, "max_tokens": MAKE_MAX_TOKENS, "messages": messages,
        "system": [{"type": "text", "text": MAKER_PROMPT,
                    "cache_control": {"type": "ephemeral", "ttl": SYSTEM_CACHE_TTL}}],
        "thinking": {"type": "adaptive"}, "output_config": {"effort": effort},
        "cache_control": {"type": "ephemeral"},
    })
}

/// The SDK's message accumulation over a stream's events, and the two kinds of event the maker reports.
#[derive(Default)]
struct Accumulated {
    message: Obj,
    content: Vec<Value>,
    partial_json: HashMap<usize, String>,
    parts: Vec<String>,
    chars: i64,
}

impl Accumulated {
    fn feed(&mut self, data: &Value, progress: &MakerProgress) -> anyhow::Result<()> {
        let index = data.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
        match data.get("type").and_then(Value::as_str).unwrap_or("") {
            "message_start" => {
                if let Some(Value::Object(m)) = data.get("message") {
                    self.message = m.clone();
                }
            }
            "content_block_start" => {
                let block = data.get("content_block").cloned().unwrap_or(Value::Null);
                if block.get("type").and_then(Value::as_str) == Some("thinking") {
                    progress("thinking", self.chars);
                }
                if index < self.content.len() {
                    self.content[index] = block;
                } else {
                    while self.content.len() < index {
                        self.content.push(Value::Null);
                    }
                    self.content.push(block);
                }
            }
            "content_block_delta" => {
                let delta = data.get("delta").cloned().unwrap_or(Value::Null);
                let field = |key: &str| delta.get(key).and_then(Value::as_str).unwrap_or("").to_string();
                let block = self.content.get_mut(index).and_then(Value::as_object_mut);
                match delta.get("type").and_then(Value::as_str).unwrap_or("") {
                    "text_delta" => {
                        let text = field("text");
                        if let Some(b) = block {
                            let now = b.get("text").and_then(Value::as_str).unwrap_or("").to_string();
                            b.insert("text".into(), Value::String(now + &text));
                        }
                        self.chars += text.chars().count() as i64;
                        self.parts.push(text);
                        progress("writing", self.chars);
                    }
                    "thinking_delta" => {
                        if let Some(b) = block {
                            let now = b.get("thinking").and_then(Value::as_str).unwrap_or("").to_string();
                            b.insert("thinking".into(), Value::String(now + &field("thinking")));
                        }
                    }
                    "signature_delta" => {
                        if let Some(b) = block {
                            b.insert("signature".into(), Value::String(field("signature")));
                        }
                    }
                    "input_json_delta" => {
                        self.partial_json.entry(index).or_default().push_str(&field("partial_json"));
                    }
                    "citations_delta" => {
                        if let (Some(b), Some(c)) = (block, delta.get("citation")) {
                            match b.get_mut("citations") {
                                Some(Value::Array(list)) => list.push(c.clone()),
                                _ => {
                                    b.insert("citations".into(), Value::Array(vec![c.clone()]));
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                if let Some(raw) = self.partial_json.remove(&index) {
                    if let Some(b) = self.content.get_mut(index).and_then(Value::as_object_mut) {
                        let input = if raw.py_trim().is_empty() {
                            json!({})
                        } else {
                            serde_json::from_str(&raw).unwrap_or(Value::String(raw))
                        };
                        b.insert("input".into(), input);
                    }
                }
            }
            "message_delta" => {
                if let Some(Value::Object(delta)) = data.get("delta") {
                    for key in ["stop_reason", "stop_sequence"] {
                        if let Some(v) = delta.get(key) {
                            self.message.insert(key.into(), v.clone());
                        }
                    }
                }
                if let Some(Value::Object(usage)) = data.get("usage") {
                    let mut merged = match self.message.get("usage") {
                        Some(Value::Object(u)) => u.clone(),
                        _ => Obj::new(),
                    };
                    for (k, v) in usage {
                        if !v.is_null() {
                            merged.insert(k.clone(), v.clone());
                        }
                    }
                    self.message.insert("usage".into(), Value::Object(merged));
                }
            }
            "error" => {
                // the SDK raises an APIStatusError with the stream's own (200) status for an error event
                return Err(eve_openrouter::Error::Status {
                    status: 200,
                    body: data.to_string(),
                    headers: Box::new(reqwest::header::HeaderMap::new()),
                }
                .into());
            }
            _ => {}
        }
        Ok(())
    }

    fn finish(self) -> Generated {
        let usage = self.message.get("usage").cloned().unwrap_or(Value::Null);
        let stop_reason = match self.message.get("stop_reason") {
            Some(Value::String(s)) => s.clone(),
            _ => String::new(),
        };
        Generated { text: self.parts.concat(), usage, stop_reason, content: Value::Array(self.content) }
    }
}

/// The `data:` of each server-sent event in a whole streamed body, up to `[DONE]`.
fn sse_data(body: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let mut data: Vec<&str> = Vec::new();
    let flush = |data: &mut Vec<&str>, out: &mut Vec<Value>| -> bool {
        if data.is_empty() {
            return true;
        }
        let raw = data.join("\n");
        data.clear();
        if raw == "[DONE]" {
            return false;
        }
        out.push(serde_json::from_str(&raw).unwrap_or(Value::String(raw)));
        true
    };
    for line in body.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            if !flush(&mut data, &mut out) {
                return out;
            }
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        if field == "data" {
            data.push(value);
        }
    }
    flush(&mut data, &mut out);
    out
}

/// One streamed turn through a lent transport: the request the Anthropic-shaped client sends (OpenRouter's
/// model id, its key and version headers), and the reply's events.
async fn stream_over(http: &Http, mut body: Value) -> anyhow::Result<Vec<Value>> {
    eve_openrouter::rewrite(&mut body);
    body["stream"] = Value::Bool(true);
    let mut headers = vec![
        ("content-type".to_string(), "application/json".to_string()),
        ("anthropic-version".to_string(), eve_openrouter::ANTHROPIC_VERSION.to_string()),
    ];
    let key = eve_openrouter::key(None);
    if !key.is_empty() {
        headers.push(("x-api-key".to_string(), key));
    }
    headers.extend(eve_openrouter::HEADERS.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    let request = HttpRequest {
        method: "POST".into(),
        url: format!("{}/v1/messages", eve_openrouter::ANTHROPIC_BASE_URL),
        headers,
        body: serde_json::to_vec(&body)?,
        timeout: Some(eve_openrouter::DEFAULT_TIMEOUT.as_secs_f64()),
    };
    let response = http.send(request).await?;
    if !(200..300).contains(&response.status) {
        let mut map = reqwest::header::HeaderMap::new();
        for (k, v) in &response.headers {
            if let (Ok(name), Ok(value)) = (
                reqwest::header::HeaderName::from_bytes(k.as_bytes()),
                reqwest::header::HeaderValue::from_str(v),
            ) {
                map.append(name, value);
            }
        }
        return Err(eve_openrouter::Error::Status { status: response.status, body: response.text(), headers: Box::new(map) }
            .into());
    }
    Ok(sse_data(&response.text()))
}

/// py: apps_maker.py:1161 `def claude_generate(model: str, effort: str = "high", client: Any = None) -> Generate:`
///
/// One maker turn: stream ``messages`` to Claude under MAKER_PROMPT, reporting "thinking", then "writing"
/// with the characters written so far.
///
/// The system prompt is cached for an hour (``SYSTEM_CACHE_TTL``): it is the same for every build, and a first
/// round writes for four minutes or more, so the default five-minute cache had expired by the repair round and
/// every repair paid to write the whole prefix again. The request's top-level breakpoint (five minutes) covers
/// the conversation, which a quick edit-style repair round reads back in time.
///
/// ``client`` is a transport to send through (a test's fake); None is OpenRouter's Anthropic-shaped client
/// (openrouter.py, 2026-10-08): "claude-opus-5-5" goes out as "anthropic/claude-opus-5.5"; adaptive thinking,
/// effort and cache_control work there as on Anthropic's API.
pub fn claude_generate(model: &str, effort: &str, client: Option<Http>) -> Generate {
    let model = model.to_string();
    let effort = effort.to_string();
    // built at the first build, so a Mini App starts before the key is read
    let api: Arc<Mutex<Option<eve_openrouter::Client>>> = Arc::new(Mutex::new(None));
    Arc::new(move |messages: Vec<Value>, progress: MakerProgress| {
        let body = maker_body(&model, &effort, messages);
        let http = client.clone();
        let api = api.clone();
        async move {
            let mut acc = Accumulated::default();
            match http {
                Some(http) => {
                    progress("thinking", 0);
                    for event in stream_over(&http, body).await? {
                        acc.feed(&event, &progress)?;
                    }
                }
                None => {
                    let existing = api.lock().clone();
                    let c = match existing {
                        Some(c) => c,
                        None => {
                            let c = eve_openrouter::anthropic_client(None, eve_openrouter::ClientOptions::default())?;
                            *api.lock() = Some(c.clone());
                            c
                        }
                    };
                    progress("thinking", 0);
                    let stream = c.messages_stream(body).await?;
                    futures::pin_mut!(stream);
                    while let Some(event) = stream.next().await {
                        acc.feed(&event?.data, &progress)?;
                    }
                }
            }
            Ok(acc.finish())
        }
        .boxed()
    })
}

/// py: apps_maker.py:1201 `def build_prompt(request: str, context: str) -> str:`
pub fn build_prompt(request: &str, context: &str) -> String {
    format!("<owner_context>\n{context}\n</owner_context>\n\nBuild this app: {}", py_strip(request))
}

/// Python's `s.strip()`.
fn py_strip(s: &str) -> &str {
    s.trim_matches(eve_transcripts::py::is_space)
}

/// The first `n` characters of `s` (Python's `s[:n]`).
fn first_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// py: apps_maker.py:1205 `def edit_prompt(app: AppInfo, html: str, data: Any, requests: list[dict[str, str]], change: str, context: str, journeys: Optional[list[jev_verify.Journey]] = None) -> str:`
pub fn edit_prompt(app: &AppInfo, html: &str, data: &Value, requests: &[HashMap<String, String>], change: &str, context: &str, journeys: Option<&[eve_jev_verify::Journey]>) -> String {
    let start = requests.len().saturating_sub(12);
    let asked = requests[start..]
        .iter()
        .map(|r| {
            let at = first_chars(r.get("at").map(String::as_str).unwrap_or(""), 16).replace('T', " ");
            format!("- {at}: {}", r.get("text").map(String::as_str).unwrap_or(""))
        })
        .collect::<Vec<_>>()
        .join("\n");
    let saved = if data.is_null() {
        "It has no saved data yet.".to_string()
    } else {
        format!(
            "Its saved data right now (what eve.load() returns; long lists and strings shortened). The new \
             version must open this as it is, migrating it in code if the shape changes:\n```json\n{}\n```",
            data_sample(data, DATA_SAMPLE_CHARS as i64)
        )
    };
    let asked = if asked.is_empty() { "- (not recorded)".to_string() } else { asked };
    let walked = match journeys {
        Some(js) if !js.is_empty() => format!(
            "Its journeys now:\n```journeys\n{}\n```\n\n",
            eve_computer_agent::py::dumps_unicode(&eve_jev_verify::journeys_json(js))
        ),
        _ => "It has no journeys yet.\n\n".to_string(),
    };
    format!(
        "<owner_context>\n{context}\n</owner_context>\n\n\
         You are changing the owner's existing app \"{}\".\n\n\
         What the owner has asked of it so far, oldest first:\n{asked}\n\n{saved}\n\n\
         The current file:\n```html\n{html}\n```\n\n\
         {walked}\
         The change the owner wants now: {}\n\n\
         Return the whole updated file, every line, in one ```html block, keeping everything they did not \
         ask to change, then the whole ```journeys block for the app as it now is, with made-up example values \
         (the saved data above stays on the Mac).",
        app.title,
        py_strip(change)
    )
}

/// py: apps_maker.py:1224 `def repair_prompt(issues: list[str], *, tested: bool = True, editable: bool = True) -> str:`
///
/// The problems, and how to answer. With a file to change (``editable``), edits: a repair of one or two
/// problems then costs a few hundred tokens and seconds, not the whole file again (25k tokens, minutes).
pub fn repair_prompt(issues: &[String], tested: bool, editable: bool) -> String {
    let listed = issues.iter().enumerate().map(|(i, text)| format!("{}. {text}", i + 1)).collect::<Vec<_>>().join("\n");
    let how = if tested {
        "I opened that file on a phone-sized screen with a stand-in Telegram and eve, filled every field, \
         tapped every button, pressed the MainButton, and reopened the app with the data it saved. "
    } else {
        ""
    };
    if !editable {
        return format!(
            "{how}These problems came up:\n{listed}\n\n\
             Fix the cause of each one and return the whole fixed file, every line, in one ```html block, then \
             its ```journeys block."
        );
    }
    format!(
        "{how}These problems came up:\n{listed}\n\n\
         Fix the cause of each one and keep everything that works. Answer with edits to the file you wrote \
         last, one block per change, in this form:\n\n\
         {EDIT_START}\n(lines copied exactly from the file, enough of them to be found only once)\n\
         {EDIT_MID}\n(the lines that replace them)\n{EDIT_END}\n\n\
         The blocks apply in order. If the fixes change most of the file, return the whole fixed file, \
         every line, in one ```html block instead.{JOURNEY_REPAIR}"
    )
}

/// A journey problem is the app's until shown otherwise: the maker may correct a journey, never weaken one.
/// py: apps_maker.py:1245 `JOURNEY_REPAIR`
pub const JOURNEY_REPAIR: &str = "

When a journey problem came from the journey itself (the app does the right thing and the journey names a control or a result in other words than the app shows), put the whole corrected ```journeys block after your fixes. Never change a journey to get around a problem in the app.";

/// py: apps_maker.py:1250 `EDIT_START`
pub const EDIT_START: &str = "<<<<<<< FIND";

/// py: apps_maker.py:1250 `EDIT_MID`
pub const EDIT_MID: &str = "=======";

/// py: apps_maker.py:1250 `EDIT_END`
pub const EDIT_END: &str = ">>>>>>> REPLACE";
/// py: apps_maker.py:1251 `_EDIT`
/// Python flags 56.
pub(crate) static EDIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?ms)^<{7} ?FIND[^\n]*\n(.*?)\n={7}[ \t]*\n(.*?)\n?>{7} ?REPLACE[^\n]*$"#).expect("EDIT"));

/// py: apps_maker.py:1254 `def apply_edits(html: str, answer: str) -> tuple[Optional[str], str]:`
///
/// ``html`` with the FIND/REPLACE blocks of ``answer`` applied in order: (the new file, "") or (None, what to
/// tell Claude). (None, "") when the answer holds no edits at all. A FIND must occur exactly once; when it
/// differs from the file only in the spaces around its lines, the one place it fits is used.
pub fn apply_edits(html: &str, answer: &str) -> (Option<String>, String) {
    let edits: Vec<(String, String)> = EDIT
        .captures_iter(answer)
        .map(|c| {
            (c.get(1).map(|m| m.as_str()).unwrap_or("").to_string(), c.get(2).map(|m| m.as_str()).unwrap_or("").to_string())
        })
        .collect();
    if edits.is_empty() {
        return (None, String::new());
    }
    let mut out = html.to_string();
    for (n, (find, replace)) in edits.iter().enumerate() {
        let where_ = format!("Edit {} of {}", n + 1, edits.len());
        if py_strip(find).is_empty() {
            return (None, format!("{where_} has nothing between {EDIT_START} and {EDIT_MID}, so it cannot be placed."));
        }
        let count = out.matches(find.as_str()).count();
        if count == 1 {
            out = out.replacen(find.as_str(), replace, 1);
            continue;
        }
        let pattern = find
            .split('\n')
            .map(|line| format!(r"[ \t]*{}[ \t]*", regex::escape(py_strip(line))))
            .collect::<Vec<_>>()
            .join("\n");
        let spans: Vec<(usize, usize)> = match Regex::new(&pattern) {
            Ok(loose) => loose.find_iter(&out).map(|m| (m.start(), m.end())).collect(),
            Err(_) => Vec::new(),
        };
        if count == 0 && spans.len() == 1 {
            let (a, b) = spans[0];
            out = format!("{}{replace}{}", &out[..a], &out[b..]);
            continue;
        }
        let found = if count > 0 { count } else { spans.len() };
        return (
            None,
            format!(
                "{where_} could not be applied: its FIND lines occur {found} times in the file, not exactly \
                 once. Copy them exactly from the file you wrote last, with enough lines to be unique, or \
                 return the whole fixed file in one ```html block."
            ),
        );
    }
    if extract_html(&out).is_none() {
        return (
            None,
            "After your edits the file is no longer one complete HTML document. Return the whole fixed \
             file in one ```html block."
                .to_string(),
        );
    }
    (Some(out), String::new())
}

/// py: apps_maker.py:1285 `_DATE_LIKE`
/// Python flags 32.
pub(crate) static DATE_LIKE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^\d{4}-\d{2}-\d{2}"#).expect("DATE_LIKE"));

/// `[^\W\d_]`: a letter.
static LETTER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^\W\d_]").expect("LETTER"));

/// `\w` in a Python `str` pattern: `isalnum()` or `_`.
fn is_word(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

/// `" ".join(v.split())`.
fn fold_spaces(v: &str) -> String {
    v.split(eve_transcripts::py::is_space).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

/// py: apps_maker.py:1288 `def owner_values(data: Any, html: str) -> set[str]:`
///
/// The words in the owner's saved data that are theirs: every string and key with a letter, 3 characters or
/// more, that the app's own page does not hold (a default category the app ships is not the owner's). Dates are
/// left out: a journey that types today's date is not the owner's record.
pub fn owner_values(data: &Value, html: &str) -> Vec<String> {
    fn consider(s: &str, page: &str, out: &mut indexmap::IndexSet<String>) {
        let t = fold_spaces(s);
        let n = t.chars().count();
        if (3..=80).contains(&n)
            && LETTER.is_match(&t)
            && !DATE_LIKE.is_match(&t)
            && !page.contains(&eve_transcripts::py::casefold(&t))
        {
            out.insert(t);
        }
    }
    fn walk(v: &Value, depth: usize, page: &str, out: &mut indexmap::IndexSet<String>) {
        if depth > 12 || out.len() > 2000 {
            return;
        }
        match v {
            Value::Object(m) => {
                for (k, x) in m {
                    // a key is a string at the next depth, as Python walks it
                    if depth < 12 && out.len() <= 2000 {
                        consider(k, page, out);
                    }
                    walk(x, depth + 1, page, out);
                }
            }
            Value::Array(items) => {
                for x in items {
                    walk(x, depth + 1, page, out);
                }
            }
            Value::String(s) => consider(s, page, out),
            _ => {}
        }
    }
    let page = eve_transcripts::py::casefold(html);
    let mut out = indexmap::IndexSet::new();
    walk(data, 0, &page, &mut out);
    out.into_iter().collect()
}

/// `re.search(r"(?<!\w)" + re.escape(value) + r"(?!\w)", text, re.I)`.
fn mentions(text: &str, value: &str) -> bool {
    let Ok(rx) = Regex::new(&format!("(?i){}", regex::escape(value))) else {
        return false;
    };
    let mut from = 0;
    while from <= text.len() {
        let Some(m) = rx.find_at(text, from) else {
            return false;
        };
        let before = text[..m.start()].chars().next_back();
        let after = text[m.end()..].chars().next();
        if !before.is_some_and(is_word) && !after.is_some_and(is_word) {
            return true;
        }
        // the next place it could start: one character on (a match the boundary refused may hide one inside it)
        from = m.start() + text[m.start()..].chars().next().map(char::len_utf8).unwrap_or(1);
    }
    false
}

/// py: apps_maker.py:1314 `def without_owner_data(journeys: list[jev_verify.Journey], data: Any, html: str, ) -> tuple[list[jev_verify.Journey], list[str]]:`
///
/// The journeys that carry none of the owner's saved data, and a problem for each that does. Jev runs off
/// the Mac (jev_verify: the step's words, the values typed and the expected text go to it), and an edit's
/// prompt shows Claude a sample of the saved data, which a journey can copy ("type Priya into Name").
pub fn without_owner_data(journeys: &[eve_jev_verify::Journey], data: &Value, html: &str) -> (Vec<eve_jev_verify::Journey>, Vec<String>) {
    let mine = owner_values(data, html);
    if mine.is_empty() {
        return (journeys.to_vec(), Vec::new());
    }
    let mut keep = Vec::new();
    let mut problems = Vec::new();
    for j in journeys {
        let mut pieces: Vec<&str> = j.steps.iter().map(|s| s.target.as_str()).collect();
        pieces.extend(j.steps.iter().map(|s| s.value.as_str()));
        pieces.extend(j.expect.iter().map(String::as_str));
        let text = pieces.join(" \n ");
        if mine.iter().any(|v| mentions(&text, v)) {
            problems.push(format!(
                "journey \"{}\" uses the owner's saved data; the verifier runs off the Mac, so write it with made-up \
                 example values. It was not walked.",
                j.name
            ));
        } else {
            keep.push(j.clone());
        }
    }
    (keep, problems)
}

/// py: apps_maker.py:1333 `def stopped_reason(e: BaseException) -> str:`
///
/// Why a build stopped on an exception, as the change chat keeps it: the Claude API's own reason where
/// there is one (a refused key, a rate limit), else that it stopped.
pub fn stopped_reason(e: &anyhow::Error) -> String {
    let line = crate::miniapp::error_line(e);
    if line.is_empty() || line == "Something went wrong answering that." {
        "the build stopped before it finished".to_string()
    } else {
        line.trim_end_matches('.').to_string()
    }
}

/// `type(e).__name__` for a log line: the SDK's class for an API failure, else the error's own kind.
fn error_kind(e: &anyhow::Error) -> String {
    if let Some(err) = e.downcast_ref::<eve_openrouter::Error>() {
        return match err {
            eve_openrouter::Error::NoKey(_) => "NoKey",
            eve_openrouter::Error::Status { status: 401, .. } => "AuthenticationError",
            eve_openrouter::Error::Status { status: 429, .. } => "RateLimitError",
            eve_openrouter::Error::Status { .. } => "APIStatusError",
            eve_openrouter::Error::Http(h) if h.is_timeout() => "APITimeoutError",
            eve_openrouter::Error::Http(_) => "APIConnectionError",
            eve_openrouter::Error::Json(_) => "JSONDecodeError",
            eve_openrouter::Error::Invalid(_) => "APIError",
        }
        .to_string();
    }
    if let Some(h) = e.downcast_ref::<HttpError>() {
        return h.kind.clone();
    }
    "Exception".to_string()
}

/// The order of a version among a build's rounds: (untested, problems, -round). A version the phone check really
/// ran on beats one it could not (a checker that did not start), whatever their counts; fewer problems next; the
/// later round on a tie.
struct Best {
    key: (bool, usize, i64),
    html: String,
    issues: Vec<String>,
    report: Option<eve_app_check::CheckReport>,
    journeys: Option<Vec<eve_jev_verify::Journey>>,
}

/// `len(s)` of a Python str.
fn chars_of(s: &str) -> i64 {
    s.chars().count() as i64
}

/// py: apps_maker.py:1343 `class AppMaker:`
///
/// Claude builds or changes one app: generate, check (in code, then in a headless phone), repair, keep the
/// best version. Every dollar goes to ``spend``; there is no cap (owner: "no claude limit, just track spend").
pub struct AppMaker {
    pub store: Arc<AppStore>,
    pub(crate) generate: Generate,
    pub(crate) cost: CostFn,
    pub(crate) spend: SpendFn,
    pub(crate) context: StrFn,
    pub(crate) check: Option<Check>,
    pub max_repairs: i64,
}

impl AppMaker {
    /// py: apps_maker.py:1347 `def __init__(self, store: AppStore, generate: Generate, *, cost: Callable[[Any], float], spend: Callable[[float], Any], context: Optional[Callable[[], str]] = None, check: Optional[Check] = app_check.check_app, max_repairs: int = 2) -> None:`
    ///
    /// ``check`` None turns the phone check off; [`real_check`] is Python's default (app_check.check_app).
    pub fn new(store: Arc<AppStore>, generate: Generate, cost: CostFn, spend: SpendFn, context: Option<StrFn>, check: Option<Check>, max_repairs: i64) -> Arc<Self> {
        let context = context.unwrap_or_else(|| Arc::new(|| owner_context(None, None)));
        Arc::new(Self { store, generate, cost, spend, context, check, max_repairs: max_repairs.max(0) })
    }

    /// py: apps_maker.py:1355 `async def make(self, request: str, progress: Progress = _no_progress) -> Made:`
    pub async fn make(self: &Arc<Self>, request: &str, progress: Option<MakerProgress>) -> Made {
        let progress = progress.unwrap_or_else(quiet);
        let prompt = build_prompt(request, &(self.context)());
        self.run(&prompt, request, "", &Value::Null, &progress, None, None).await
    }

    /// py: apps_maker.py:1358 `async def edit(self, name: str, change: str, progress: Progress = _no_progress) -> Made:`
    pub async fn edit(self: &Arc<Self>, name: &str, change: &str, progress: Option<MakerProgress>) -> Made {
        let progress = progress.unwrap_or_else(quiet);
        // A slug is taken as it is (the Mini App and the chat door both pass one): the door locked that app,
        // so the build must change that app and no other that happens to share its name.
        let Some(app) = self.store.info(name).or_else(|| self.store.find(name)) else {
            return Made::failed(format!("there is no app called {}", eve_app_registry::py_str_repr(name)));
        };
        let html = self.store.html(&app.slug).unwrap_or_default();
        // gone meanwhile (KeyError): no data
        let data = self.store.load_data(&app.slug).unwrap_or(Value::Null);
        let journeys = self.store.journeys(&app.slug);
        let prompt =
            edit_prompt(&app, &html, &data, &self.store.requests(&app.slug), change, &(self.context)(), journeys.as_deref());
        let t0 = Instant::now();
        let tally = Mutex::new(Obj::new());
        {
            let mut t = tally.lock();
            t.insert("usd".into(), json!(0.0));
            t.insert("rounds".into(), json!(0));
        }
        let made = self.run(&prompt, change, &app.slug, &data, &progress, journeys.as_deref(), Some(&tally)).await;
        if let Some(e) = &made.error {
            // Claude could not be reached: the change chat still shows the request, why it did nothing, and what
            // the rounds before the failure already cost
            let (usd, rounds) = {
                let t = tally.lock();
                (t.get("usd").and_then(Value::as_f64).unwrap_or(0.0), t.get("rounds").and_then(Value::as_i64).unwrap_or(0))
            };
            let failed = Made {
                reason: stopped_reason(e),
                usd,
                secs: t0.elapsed().as_secs_f64(),
                rounds,
                ..Made::default()
            };
            self.record_failed(&app.slug, change, &failed);
            return made;
        }
        if !made.ok {
            self.record_failed(&app.slug, change, &made);
        }
        made
    }

    /// py: apps_maker.py:1385 `def _record_failed(self, slug: str, change: str, made: Made) -> None:`
    pub(crate) fn record_failed(self: &Arc<Self>, slug: &str, change: &str, made: &Made) {
        let record =
            build_record(false, made.check.as_ref(), &made.issues, made.usd, made.secs, made.rounds, &made.reason);
        if let Err(e) = self.store.record_build(slug, change, &record) {
            // deleted meanwhile, or the disk: never fails the build
            let kind = match e {
                StoreError::Missing(_) => "KeyError",
                StoreError::Io(_) => "OSError",
                StoreError::Invalid(_) => "ValueError",
                StoreError::Exists(_) => "FileExistsError",
            };
            tracing::info!("apps: the failed build of {slug} was not recorded ({kind})");
        }
    }

    /// py: apps_maker.py:1393 `async def _test(self, html: str, seed: Any, journeys: Optional[list[jev_verify.Journey]] = None, ) -> tuple[list[str], Optional[app_check.CheckReport]]:`
    pub(crate) async fn test(self: &Arc<Self>, html: &str, seed: &Value, journeys: Option<&[eve_jev_verify::Journey]>) -> (Vec<String>, Option<eve_app_check::CheckReport>) {
        let issues = static_issues(html);
        let Some(check) = &self.check else {
            return (issues, None);
        };
        // journeys only when there are some: a check written before them takes (html, seed=) alone
        let opts = eve_app_check::CheckOptions {
            seed: if seed.is_null() { None } else { Some(seed.clone()) },
            journeys: journeys.filter(|j| !j.is_empty()).map(<[eve_jev_verify::Journey]>::to_vec),
            ..eve_app_check::CheckOptions::default()
        };
        let report = check(html.to_string(), opts).await;
        let mut all = issues.clone();
        all.extend(report.issues.iter().filter(|i| !issues.contains(i)).cloned());
        (all, Some(report))
    }

    /// py: apps_maker.py:1407 `async def _run(self, prompt: str, request: str, slug: str, seed: Any, progress: Progress, journeys: Optional[list[jev_verify.Journey]], tally: Optional[dict[str, Any]] = None) -> Made:`
    ///
    /// The build loop. ``journeys`` are the ones in hand (an edit's current ones, else None): an answer with a
    /// usable ```journeys block replaces them, one without keeps them, and the version kept is saved with the
    /// journeys it was checked with. A block that is empty or does not parse keeps them too, and stays a
    /// problem until a usable one comes: otherwise it would switch the verifier off for the app. A repair
    /// round's block never weakens the set: a journey it leaves out is still walked. On an edit (``seed``, the
    /// saved data), a journey that carries the owner's data is never walked (without_owner_data).
    /// ``tally`` gets the running cost ("usd") and rounds ("rounds"), for a caller whose build raises part way.
    /// Where Python raised (a first round that failed), the Made carries the error ([`Made::error`]).
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn run(self: &Arc<Self>, prompt: &str, request: &str, slug: &str, seed: &Value, progress: &MakerProgress, journeys: Option<&[eve_jev_verify::Journey]>, tally: Option<&Mutex<Obj>>) -> Made {
        let note = |key: &str, value: Value| {
            if let Some(t) = tally {
                t.lock().insert(key.to_string(), value);
            }
        };
        let t0 = Instant::now();
        let mut messages: Vec<Value> = vec![json!({"role": "user", "content": prompt})];
        let mut usd = 0.0;
        let mut best: Option<Best> = None;
        let mut why = "Claude did not return a complete app".to_string();
        let mut issues: Vec<String> = Vec::new();
        let mut history: Vec<Vec<String>> = Vec::new();
        // the file Claude wrote last: what a repair's edits apply to
        let mut last: Option<String> = None;
        // an unusable journeys block's problems, until a usable one comes
        let mut stuck: Vec<String> = Vec::new();
        let mut journeys: Option<Vec<eve_jev_verify::Journey>> = journeys.filter(|j| !j.is_empty()).map(<[_]>::to_vec);
        let assistants = |messages: &[Value]| messages.iter().filter(|m| m["role"] == "assistant").count() as i64;
        for round_no in 0..=self.max_repairs {
            let step: MakerProgress = if round_no > 0 {
                let n = issues.len() as i64;
                progress("fixing", n);
                let p = progress.clone();
                Arc::new(move |_stage: &str, _chars: i64| p("fixing", n))
            } else {
                progress.clone()
            };
            let gen = match (self.generate)(messages.clone(), step).await {
                Ok(gen) => gen,
                Err(e) => {
                    if best.is_none() {
                        let reason = stopped_reason(&e);
                        return Made {
                            ok: false,
                            reason,
                            usd,
                            secs: t0.elapsed().as_secs_f64(),
                            rounds: assistants(&messages),
                            history,
                            error: Some(Arc::new(e)),
                            ..Made::default()
                        };
                    }
                    tracing::warn!("apps: a repair round failed; keeping the best version so far: {e:#}");
                    break;
                }
            };
            let spent = if gen.usage.is_null() { 0.0 } else { (self.cost)(&gen.usage) };
            if !gen.usage.is_null() {
                (self.spend)(spent);
            }
            usd += spent;
            note("usd", json!(usd));
            if gen.stop_reason == "refusal" {
                why = "Claude declined to build that".to_string();
                break;
            }
            let (mut html, bad_edit) = match &last {
                Some(last) => apply_edits(last, &gen.text),
                None => (None, String::new()),
            };
            if html.is_none() && bad_edit.is_empty() {
                html = extract_html(&gen.text);
            }
            let (found, mut journey_problems) = eve_jev_verify::extract_journeys(&gen.text);
            let had_block = found.is_some();
            match found {
                Some(mut found) if !found.is_empty() => {
                    if round_no > 0 {
                        if let Some(held) = &journeys {
                            let names: HashSet<String> = found.iter().map(|j| j.name.clone()).collect();
                            found.extend(held.iter().filter(|j| !names.contains(&j.name)).cloned());
                            found.truncate(eve_jev_verify::MAX_JOURNEYS);
                        }
                    }
                    journeys = Some(found);
                    stuck = Vec::new();
                }
                // the journeys in hand stay
                Some(_) => stuck = journey_problems.clone(),
                None => {
                    if !stuck.is_empty() {
                        journey_problems = stuck.clone();
                    }
                }
            }
            if html.is_none() && bad_edit.is_empty() && last.is_some() && had_block {
                html = last.clone(); // a repair that corrected only the journeys
            }
            let tested;
            match &html {
                None => {
                    let cut = gen.stop_reason == "max_tokens";
                    why = if cut { "the app was too long to finish" } else { "Claude did not return a complete app" }
                        .to_string();
                    let line = if !bad_edit.is_empty() {
                        bad_edit.clone()
                    } else if cut {
                        "Your answer was cut off at the length limit before the file was complete. Write it again \
                         more compactly (shorter CSS, fewer comments, no repetition) so it fits."
                            .to_string()
                    } else {
                        "Your answer did not contain one complete HTML document in a ```html block.".to_string()
                    };
                    issues = vec![line];
                    tested = false;
                }
                Some(html) => {
                    last = Some(html.clone());
                    if !seed.is_null() {
                        if let Some(held) = &journeys {
                            let (kept, leaked) = without_owner_data(held, seed, html);
                            journeys = if kept.is_empty() { None } else { Some(kept) };
                            journey_problems.extend(leaked);
                        }
                    }
                    progress("testing", chars_of(html));
                    let (found_issues, report) = self.test(html, seed, journeys.as_deref()).await;
                    if journeys.is_none() && journey_problems.is_empty() {
                        // not a problem to repair (a round costs more than it proves): the build is checked by the
                        // script alone, as an app from before journeys is
                        tracing::info!("apps: round {round_no} came with no journeys; only the scripted check runs");
                    }
                    issues = found_issues;
                    issues.extend(journey_problems.iter().map(|p| format!("Journeys block: {p}")));
                    tested = report.as_ref().is_some_and(|r| r.skipped.is_empty());
                    let untested = self.check.is_some() && !tested;
                    let key = (untested, issues.len(), -round_no);
                    if !load_issues(html).is_empty() {
                        // never kept: a script from outside would run with the owner's data (load_issues)
                        why = "the app kept loading code from outside cdn.jsdelivr.net/npm at an exact version".to_string();
                    } else if best.as_ref().is_none_or(|b| key < b.key) {
                        best = Some(Best {
                            key,
                            html: html.clone(),
                            issues: issues.clone(),
                            report,
                            journeys: journeys.clone(),
                        });
                    }
                }
            }
            history.push(issues.clone());
            if html.is_some() && issues.is_empty() {
                break;
            }
            tracing::info!(
                "apps: round {round_no} found {} problem(s): {}",
                issues.len(),
                first_chars(&issues.join("; "), 500)
            );
            if round_no == self.max_repairs {
                break;
            }
            let answered = if gen.content.is_null() { Value::String(gen.text.clone()) } else { gen.content.clone() };
            messages.push(json!({"role": "assistant", "content": answered}));
            messages.push(json!({"role": "user", "content": repair_prompt(&issues, tested, last.is_some())}));
            note("rounds", json!(assistants(&messages)));
        }
        let secs = t0.elapsed().as_secs_f64();
        let rounds = assistants(&messages);
        let Some(best) = best else {
            return Made { ok: false, reason: why, usd, secs, rounds, history, ..Made::default() };
        };
        let Best { html, issues: left, report, journeys: kept_journeys, .. } = best;
        if !slug.is_empty() && self.store.info(slug).is_none() {
            return Made {
                ok: false,
                reason: "the app was deleted while it was being changed".to_string(),
                usd,
                secs,
                check: report,
                rounds,
                issues: left,
                history,
                ..Made::default()
            };
        }
        progress("saving", chars_of(&html));
        let record = build_record(true, report.as_ref(), &left, usd, secs, rounds, "");
        let app = match self.store.save(&html, request, slug, kept_journeys.as_deref(), Some(&record)) {
            Ok(app) => app,
            Err(e) => {
                // Python raised out of the build: a bad name or the disk
                let e = anyhow::Error::new(e);
                return Made {
                    ok: false,
                    reason: stopped_reason(&e),
                    usd,
                    secs,
                    check: report,
                    rounds,
                    issues: left,
                    history,
                    error: Some(Arc::new(e)),
                    ..Made::default()
                };
            }
        };
        tracing::info!(
            "apps: {} {} in {:.0} s, {} repair round(s), {} problem(s) left, ${:.3}",
            if slug.is_empty() { "made" } else { "edited" },
            app.slug,
            secs,
            rounds,
            left.len(),
            usd
        );
        Made { ok: true, app: Some(app), usd, secs, check: report, rounds, issues: left, history, error: None, ..Made::default() }
    }
}

// ---- window.eve, injected into every app --------------------------------------------------------

// Saves go out one at a time and the newest value wins: three quick taps are at most one request in flight plus
// one waiting (holding the latest value), so a slow earlier save can never land after a newer one. Each is sent
// with keepalive when small enough (the browser's limit is 64 KB), so a save started just before Telegram
// closes the app still arrives.
//
// The page is sandboxed (miniapp.APP_CSP): its requests come from an opaque origin, so they are sent as
// text/plain, which keeps them CORS-simple (no preflight, which keepalive requests cannot always make). They
// carry the app token the page was opened with (``?t=``, miniapp.app_token), read once before the app's own
// code runs; it answers for this app's load and save and nothing else. The app never has the owner's initData.
//
// Telegram's Back button also leads home: on the app's own first view (where the app hides the button) it is
// kept showing and goes back to eve's list of apps, so the owner can reach Change, Undo and the other apps
// without closing the Mini App. On any other view the app's own handlers run, as the app registered them.
/// py: apps_maker.py:1537 `EVE_JS`
pub const EVE_JS: &str = "(() => {
  const slug = location.pathname.split(\"/\").filter(Boolean)[1] || \"\";
  const token = new URLSearchParams(location.search).get(\"t\") || \"\";
  const body = (rest) => '{\"t\":' + JSON.stringify(token) + rest + \"}\";
  async function post(path, raw) {
    const r = await fetch(path, { method: \"POST\", headers: { \"Content-Type\": \"text/plain\" }, body: raw,
      keepalive: raw.length < 60000 });
    const j = await r.json().catch(() => ({}));
    if (!r.ok) throw new Error(j.error || (\"HTTP \" + r.status));
    return j;
  }
  let busy = false, next = null;       // next: { data, waiting: [[resolve, reject], ...] }, the newest value
  function pump() {
    if (busy || !next) return;
    const job = next; next = null; busy = true;
    post(\"/api/apps/\" + slug + \"/save\", body(',\"data\":' + job.data))
      .then(() => job.waiting.forEach((w) => w[0](true)), (e) => job.waiting.forEach((w) => w[1](e)))
      .finally(() => { busy = false; pump(); });
  }
  window.eve = {
    slug,
    load: async () => (await post(\"/api/apps/\" + slug + \"/load\", body(\"\"))).data ?? null,
    save: (value) => new Promise((resolve, reject) => {
      const data = JSON.stringify(value);  // a snapshot now: later changes to the object are the next save's
      if (data === undefined) { reject(new Error(\"eve.save needs a JSON value\")); return; }
      next = next || { waiting: [] };
      next.data = data; next.waiting.push([resolve, reject]);
      pump();
    }),
  };
  // What this page does on the real phone, sent once when it has settled and again after a new error (at most 3
  // reports): the Telegram it runs in, whether Telegram's native bridge is here (without it the MainButton and
  // BackButton do nothing), the MainButton's state, and the page's errors. The server logs it (phone_report).
  const errors = [];
  let reports = 0, timer = null;
  function report() {
    timer = null;
    if (reports >= 3) return;
    reports++;
    const w = window.Telegram && window.Telegram.WebApp, mb = w && w.MainButton;
    const bridge = window.TelegramWebviewProxy ? \"proxy\" : (window.external && window.external.notify) ? \"external\"
      : (window.parent !== window) ? \"iframe\" : \"none\";
    const info = { platform: w ? w.platform : \"\", version: w ? w.version : \"\", bridge,
      main_button: mb ? (mb.isVisible ? \"shown \" + JSON.stringify(mb.text || \"\") : \"hidden\") : \"\", errors: errors.slice(-5) };
    post(\"/api/apps/\" + slug + \"/report\", body(\",\" + JSON.stringify(info).slice(1, -1))).catch(() => {});
  }
  const later = (ms) => { if (!timer) timer = setTimeout(report, ms); };
  window.addEventListener(\"error\", (e) => { errors.push(String(e.message || e.error || \"error\") + (e.lineno ? \" @\" + e.lineno : \"\")); later(1000); });
  window.addEventListener(\"unhandledrejection\", (e) => { errors.push(\"promise: \" + String((e.reason && e.reason.message) || e.reason)); later(1000); });
  window.addEventListener(\"load\", () => later(3000));
  const tg = window.Telegram && window.Telegram.WebApp, bb = tg && tg.BackButton;
  if (bb && tg.isVersionAtLeast && tg.isVersionAtLeast(\"6.1\")) {
    const show = bb.show, on = bb.onClick, own = [];
    let appShows = false;              // the app wants Back (a view below its first): its handlers run
    on.call(bb, () => { if (appShows) own.slice().forEach((f) => f()); else location.href = \"/\"; });
    show.call(bb);
    bb.show = () => { appShows = true; return bb; };
    bb.hide = () => { appShows = false; return bb; };
    bb.onClick = (f) => { if (typeof f === \"function\" && !own.includes(f)) own.push(f); return bb; };
    bb.offClick = (f) => { const i = own.indexOf(f); if (i >= 0) own.splice(i, 1); return bb; };
  }
  // The way to change this app: its chat on eve's home page (/?chat=<slug>), where the owner's signed
  // initData lives. The app only navigates there; it is never given the power to change apps or spend.
  // Telegram's Settings item in the Mini App's \"...\" menu (7.0+) goes there, and so does a small pencil in the
  // page's top right corner, beside the app's title, that scrolls away with the page (the maker is told to
  // keep that corner free, and app_check reports a control under it). The pencil lives in a
  // closed shadow root on <html>, with its host's position set inline and !important, so the app's own CSS
  // (a \"button { ... }\" rule, a body that is re-rendered) cannot restyle or remove it by accident.
  const toChat = () => { location.href = \"/?chat=\" + encodeURIComponent(slug); };
  const sb = tg && tg.SettingsButton;
  if (sb && tg.isVersionAtLeast && tg.isVersionAtLeast(\"7.0\")) {
    try { sb.onClick(toChat); sb.show(); } catch (e) {}
  }
  function pencil() {
    if (!slug || document.querySelector(\"eve-change\")) return;
    const host = document.createElement(\"eve-change\");
    host.setAttribute(\"style\", \"all: initial !important; position: absolute !important; z-index: 2147483647 !important; \"
      + \"display: block !important; width: 44px !important; height: 44px !important; \"
      + \"top: calc(6px + var(--tg-safe-area-inset-top, 0px) + var(--tg-content-safe-area-inset-top, 0px)) !important; \"
      + \"right: calc(6px + var(--tg-safe-area-inset-right, 0px) + var(--tg-content-safe-area-inset-right, 0px)) !important;\");
    const root = host.attachShadow({ mode: \"closed\" });
    root.innerHTML = '<style>button { all: initial; box-sizing: border-box; width: 44px; height: 44px; display: grid; '
      + 'place-items: center; cursor: pointer; border-radius: 22px; -webkit-tap-highlight-color: transparent; } '
      + 'span { width: 32px; height: 32px; border-radius: 16px; display: grid; place-items: center; '
      + 'font: 17px/1 -apple-system, system-ui, sans-serif; opacity: .8; '
      + 'background: var(--tg-theme-secondary-bg-color, rgba(127, 127, 127, .16)); '
      + 'color: var(--tg-theme-link-color, #2481cc); } '
      + 'button:focus-visible span { outline: 2px solid var(--tg-theme-link-color, #2481cc); outline-offset: 2px; } '
      + '</style><button type=\"button\" aria-label=\"Change this app\" title=\"Change this app\">'
      + '<span aria-hidden=\"true\">✎</span></button>';
    const btn = root.querySelector(\"button\");
    btn.addEventListener(\"click\", toChat);
    document.documentElement.appendChild(host);
    // The corner belongs to the app first: while one of its own controls is under the pencil (a header's
    // Settings button, a dialog's close), the pencil is hidden, so a tap there reaches the app. Checked again
    // after every change to the page, a scroll or a resize, at most once a frame.
    const TAP = \"button,a[href],input,select,textarea,label,summary,[role=button],[role=tab],[role=link],\"
      + \"[role=checkbox],[role=switch],[role=radio],[role=menuitem],[role=option],[onclick]\";
    let queued = false, hidden = false;
    const under = () => {
      const r = host.getBoundingClientRect();
      if (r.bottom <= 0 || r.top >= innerHeight) return hidden;      // scrolled away: nothing to decide
      const pts = [[r.left + r.width / 2, r.top + r.height / 2], [r.left + 8, r.top + 8], [r.right - 8, r.top + 8],
        [r.left + 8, r.bottom - 8], [r.right - 8, r.bottom - 8]];
      return pts.some(([x, y]) => {
        const el = document.elementsFromPoint(x, y).find((e) => e !== host);
        return !!(el && el !== document.documentElement && el !== document.body && el.closest(TAP));
      });
    };
    const settle = () => {
      queued = false;
      const now = under();
      if (now === hidden) return;
      hidden = now;                    // the host and its button both: the button's \"all: initial\" is visible
      [host, btn].forEach((e) => e.style.setProperty(\"visibility\", now ? \"hidden\" : \"visible\", \"important\"));
    };
    const later = () => { if (!queued) { queued = true; requestAnimationFrame(settle); } };
    new MutationObserver(later).observe(document.documentElement, { subtree: true, childList: true, attributes: true,
      characterData: true });
    addEventListener(\"scroll\", later, { passive: true, capture: true });
    addEventListener(\"resize\", later);
    settle();
  }
  if (document.readyState === \"loading\") document.addEventListener(\"DOMContentLoaded\", pencil); else pencil();
})();
";

/// `<head ...>`, case-insensitively.
static HEAD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<head[^>]*>").expect("HEAD"));

/// py: apps_maker.py:1665 `def serve_app_html(html: str) -> str:`
///
/// The app as served: Telegram's script and window.eve put in the head, ahead of the app's own code.
pub fn serve_app_html(html: &str) -> String {
    let mut inject = String::new();
    if !html.contains("telegram-web-app.js") {
        inject.push_str(r#"<script src="https://telegram.org/js/telegram-web-app.js"></script>"#);
    }
    inject.push_str(r#"<script src="/eve.js"></script>"#);
    match HEAD.find(html) {
        Some(m) => format!("{}{inject}{}", &html[..m.end()], &html[m.end()..]),
        None => inject + html,
    }
}

// ---- the chat door: "make me a habit tracker" texted to eve ---------------------------------------

/// py: apps_maker.py:1677 `def _app_arg(description: str = "Which app, by its name.") -> dict[str, Any]:`
pub fn app_arg(description: &str) -> Obj {
    let mut out = Obj::new();
    out.insert("type".into(), json!("string"));
    out.insert("description".into(), json!(description));
    out
}

/// py: apps_maker.py:1681 `MAKER_TOOLS`
/// The Python value as JSON (tuples are arrays).
pub static MAKER_TOOLS: LazyLock<Value> =
    LazyLock::new(|| serde_json::from_str(r#"[{"type": "function", "name": "make_app", "strict": true, "description": "Build a NEW small app (a tracker, a log, a list, a calculator, a planner…) that opens inside Telegram and saves its data. Use it whenever the owner asks for an app, tracker or tool to be made. Takes a few minutes; the app's picture and an Open button are sent to the chat when it is ready.", "parameters": {"type": "object", "additionalProperties": false, "required": ["request"], "properties": {"request": {"type": "string", "description": "What the app should do, in the owner's words plus any detail they gave."}}}}, {"type": "function", "name": "change_app", "strict": true, "description": "Change one of the owner's EXISTING apps: add a feature, fix something, restyle it. Its saved data is kept. The new version's picture and Open button are sent when it is ready.", "parameters": {"type": "object", "additionalProperties": false, "required": ["app", "change"], "properties": {"app": {"type": "string", "description": "Which app, by its name."}, "change": {"type": "string", "description": "What to change."}}}}, {"type": "function", "name": "undo_app", "strict": true, "description": "Put one of the owner's apps back to how it was before its last change (undo, revert, go back, \"the last change broke it\"). Its saved data is untouched. When the name fits no app or more than one, nothing is undone and the candidates come back: ask which.", "parameters": {"type": "object", "additionalProperties": false, "required": ["app"], "properties": {"app": {"type": "string", "description": "Which app, by its name."}}}}, {"type": "function", "name": "rename_app", "strict": true, "description": "Give one of the owner's apps a new name. Only the name changes.", "parameters": {"type": "object", "additionalProperties": false, "required": ["app", "title"], "properties": {"app": {"type": "string", "description": "Which app, by its current name."}, "title": {"type": "string", "description": "The new name."}}}}, {"type": "function", "name": "delete_app", "strict": true, "description": "Delete one of the owner's apps when they ask to delete or remove it. Code asks for approval before deletion. It moves to eve's trash and restore_app brings it back. When the name fits no app or more than one, nothing is deleted and the candidates come back: ask which.", "parameters": {"type": "object", "additionalProperties": false, "required": ["app"], "properties": {"app": {"type": "string", "description": "Which app, by its name."}}}}, {"type": "function", "name": "restore_app", "strict": true, "description": "Bring back an app the owner deleted (\"undelete\", \"I deleted it by mistake\"), with its saved data.", "parameters": {"type": "object", "additionalProperties": false, "required": ["app"], "properties": {"app": {"type": "string", "description": "Which deleted app, by its name."}}}}, {"type": "function", "name": "list_apps", "strict": true, "description": "The apps eve has made for the owner, and a button to open each.", "parameters": {"type": "object", "additionalProperties": false, "required": [], "properties": {}}}]"#).expect("MAKER_TOOLS"));

/// py: apps_maker.py:1736 `MAKER_TOOL_NAMES`
/// A Python set: sorted here; membership is what matters.
pub const MAKER_TOOL_NAMES: &[&str] = &["change_app", "delete_app", "list_apps", "make_app", "rename_app", "restore_app", "undo_app"];

// py: apps_maker.py:1738 `SendButtons`, 1739 `SendPhoto`: crate::seams::{SendButtons, SendPhoto}.

/// Python's truthiness of a tool argument.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|x| x != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(m)) => !m.is_empty(),
    }
}

/// `str(args.get(a) or args.get(b) or "")`: the first truthy argument, as Python's str() prints it.
fn arg_text(args: &Obj, keys: &[&str]) -> String {
    match keys.iter().map(|k| args.get(*k)).find(|v| truthy(*v)).flatten() {
        Some(v) => eve_computer_agent::py::value_str(v),
        None => String::new(),
    }
}

/// py: apps_maker.py:1742 `class ChatMaker:`
///
/// The Telegram brain's app tools, over a live Mini App (miniapp.MiniApp).
///
/// A build runs in the background like a Mac task: the tool returns at once, and when it is done the app's
/// screenshot (from the check) goes to the chat with an Open button under it (an inline ``web_app`` button,
/// so it opens as a Mini App with the owner's signed initData); a change also gets a Change button to the
/// app's change chat on the home page, to keep refining it. Undo, rename, delete and restore are instant;
/// the first three act only on an app the owner named for sure (AppStore.resolve), never on a guess.
pub struct ChatMaker {
    pub(crate) app: Arc<dyn MakerHost>,
    pub(crate) spawn: SpawnFn,
    pub building: Mutex<HashSet<String>>,
    /// CHAT_PROGRESS_SECS, the module constant Python's tests patch (to 0); [`ChatMaker::set_progress_secs`].
    progress_secs: Mutex<f64>,
}

impl ChatMaker {
    /// py: apps_maker.py:1751 `def __init__(self, app: Any, spawn: Callable[[Awaitable[Any], str], Any]) -> None:`
    pub fn new(app: Arc<dyn MakerHost>, spawn: SpawnFn) -> Arc<Self> {
        // miniapp.MiniApp: .maker, .store, .app_url(slug), .chat_url(slug), .url
        Arc::new(Self { app, spawn, building: Mutex::new(HashSet::new()), progress_secs: Mutex::new(CHAT_PROGRESS_SECS) })
    }

    /// The shortest gap between two progress lines (CHAT_PROGRESS_SECS unless set: Python's tests patch the
    /// module constant).
    pub fn set_progress_secs(&self, secs: f64) {
        *self.progress_secs.lock() = secs;
    }

    /// py: apps_maker.py:1757 `def live(self) -> bool:`
    pub fn live(self: &Arc<Self>) -> bool {
        !self.app.url().is_empty()
    }

    /// py: apps_maker.py:1760 `def tools(self) -> list[dict[str, Any]]:`
    pub fn tools(self: &Arc<Self>) -> Vec<Obj> {
        if !self.live() {
            return Vec::new();
        }
        MAKER_TOOLS.as_array().map(|a| a.iter().filter_map(|t| t.as_object().cloned()).collect()).unwrap_or_default()
    }

    /// py: apps_maker.py:1763 `def home_url(self) -> str:`
    ///
    /// The Mini App's home screen right now ("" while the tunnel is down).
    pub fn home_url(self: &Arc<Self>) -> String {
        if self.live() { format!("{}/", self.app.url()) } else { String::new() }
    }

    /// py: apps_maker.py:1767 `async def stick_link(self, rotate: bool = False) -> str:`
    ///
    /// The owner's /stick (stick_link.py): Eve Link's pairing link, "" while the tunnel is down.
    pub async fn stick_link(self: &Arc<Self>, rotate: bool) -> String {
        if self.live() { self.app.stick_link(rotate).await } else { String::new() }
    }

    /// py: apps_maker.py:1771 `def _missing(self, target: str, candidates: Optional[list[AppInfo]] = None) -> dict[str, Any]:`
    ///
    /// Nothing was done: the name fits no app, or more than one. The brain asks the owner which.
    pub(crate) fn missing(self: &Arc<Self>, target: &str, candidates: Option<&[AppInfo]>) -> Obj {
        let quoted = eve_app_registry::py_str_repr(target);
        let mut out = Obj::new();
        out.insert("ok".into(), json!(false));
        if let Some(candidates) = candidates.filter(|c| !c.is_empty()) {
            let named: Vec<String> = candidates
                .iter()
                .take(8)
                .map(|a| {
                    if candidates.iter().filter(|c| c.title == a.title).count() == 1 {
                        a.title.clone()
                    } else {
                        format!("{} ({})", a.title, a.slug)
                    }
                })
                .collect();
            out.insert(
                "reason".into(),
                json!(format!(
                    "{quoted} could mean more than one app; nothing was done. Ask the owner which, then call \
                     again with its exact name."
                )),
            );
            out.insert("candidates".into(), json!(named));
            return out;
        }
        let names = self.app.store().list().iter().map(|a| a.title.clone()).collect::<Vec<_>>().join(", ");
        let names = if names.is_empty() { "none yet".to_string() } else { names };
        out.insert("reason".into(), json!(format!("no app called {quoted}; nothing was done. The apps are: {names}")));
        out
    }

    /// py: apps_maker.py:1782 `async def handle(self, name: str, args: dict[str, Any], chat_id: int, send: SendButtons, say: Callable[[int, str], Awaitable[Any]], send_photo: Optional[SendPhoto] = None ) -> dict[str, Any]:`
    ///
    /// A send that fails ends the tool as the brain's own handler ends a tool that raised: `{name} failed`.
    pub async fn handle(self: &Arc<Self>, name: &str, args: &Obj, chat_id: i64, send: SendButtons, say: SayFn, send_photo: Option<SendPhoto>) -> Obj {
        match self.handle_inner(name, args, chat_id, send, say, send_photo).await {
            Ok(out) => out,
            Err(e) => {
                tracing::error!("telegram: {name} failed: {e:#}");
                obj(json!({"ok": false, "reason": format!("{name} failed")}))
            }
        }
    }

    async fn handle_inner(self: &Arc<Self>, name: &str, args: &Obj, chat_id: i64, send: SendButtons, say: SayFn, send_photo: Option<SendPhoto>) -> anyhow::Result<Obj> {
        if !self.live() {
            return Ok(obj(json!({"ok": false, "reason": "the Mini App is not running right now"})));
        }
        let store = self.app.store();
        if name == "list_apps" {
            let apps = store.list();
            if apps.is_empty() {
                return Ok(obj(json!({"ok": true, "apps": [], "note": "no apps yet"})));
            }
            let buttons = apps
                .iter()
                .take(8)
                .map(|a| (py_strip(&format!("{} {}", a.icon, a.title)).to_string(), self.app.app_url(&a.slug)))
                .collect();
            send(chat_id, "Your apps:".to_string(), buttons).await?;
            let titles: Vec<String> = apps.iter().map(|a| a.title.clone()).collect();
            return Ok(obj(json!({"ok": true, "apps": titles, "sent": "the buttons are in the chat already"})));
        }
        let mut target = py_strip(&arg_text(args, &["app"])).to_string();
        if name == "restore_app" {
            return self.restore(&target, chat_id, &send).await;
        }
        if matches!(name, "delete_app" | "rename_app" | "undo_app") {
            // Acted on at once, so never on a guess: "delete the water app" with Water Log and Water Plants,
            // or "undo the water tracker" when only a Habit Tracker is left, does nothing and asks.
            let (app, candidates) = if target.is_empty() { (None, Vec::new()) } else { store.resolve(&target) };
            let Some(app) = app else {
                return Ok(self.missing(&target, Some(&candidates)));
            };
            {
                let building = self.building.lock();
                if building.contains(&app.slug) || building.contains(&app.title) {
                    return Ok(obj(json!({"ok": false,
                        "reason": format!("{} is being changed right now; try again when it is done", app.title)})));
                }
            }
            if name == "delete_app" {
                store.delete(&app.slug)?;
                return Ok(obj(json!({"ok": true, "deleted": app.title,
                    "note": "moved to eve's trash (apps/.trash on the Mac) with its data; restore_app brings it back"})));
            }
            if name == "rename_app" {
                let renamed = match store.rename(&app.slug, &arg_text(args, &["title"])) {
                    Ok(renamed) => renamed,
                    Err(StoreError::Invalid(why)) => return Ok(obj(json!({"ok": false, "reason": why}))),
                    Err(e) => return Err(e.into()),
                };
                return Ok(obj(json!({"ok": true, "renamed": {"from": app.title, "to": renamed.title}})));
            }
            let back = match store.revert(&app.slug) {
                Ok(back) => back,
                Err(StoreError::Missing(_)) => {
                    return Ok(obj(json!({"ok": false,
                        "reason": format!("{} has no earlier version to go back to", app.title)})));
                }
                Err(e) => return Err(e.into()),
            };
            let url = self.app.app_url(&back.slug);
            let mut line = format!("{} is back to how it was before its last change.", back.title);
            if app.undo_warning {
                line.push_str(
                    " Entries saved since that change may not open in this version; changing the app again \
                     brings them back.",
                );
            }
            if !url.is_empty() {
                send(chat_id, line, vec![(format!("Open {}", back.title), url)]).await?;
            }
            let mut out = obj(json!({"ok": true, "undone": back.title, "earlier_versions_left": back.versions,
                                     "sent": "an Open button is in the chat already"}));
            if app.undo_warning {
                out.insert(
                    "note".into(),
                    json!("its saved data has a newer shape than this version knows; said so in the chat"),
                );
            }
            return Ok(out);
        }
        let request = py_strip(&arg_text(args, &["request", "change"])).to_string();
        if name != "change_app" {
            target = String::new();
        }
        if request.is_empty() {
            return Ok(obj(json!({"ok": false, "reason": "say what the app should do"})));
        }
        if !target.is_empty() {
            let Some(found) = store.find(&target) else {
                return Ok(self.missing(&target, None));
            };
            target = found.slug;
        }
        let key = if target.is_empty() { request.clone() } else { target.clone() };
        {
            let mut building = self.building.lock();
            if building.contains(&key) {
                return Ok(obj(json!({"ok": false, "reason": "that one is already being built"})));
            }
            building.insert(key.clone());
        }
        let me = self.clone();
        let job = async move { me.build(chat_id, &request, &target, &key, send, say, send_photo).await }.boxed();
        (self.spawn)(job, "apps-build");
        Ok(obj(json!({"ok": true, "sent": "building now (a few minutes, tested on a phone before it is sent); a short \
                                         line comes when it is being tested, and the app's picture and Open button \
                                         when it is ready — say only a word or two"})))
    }

    /// py: apps_maker.py:1852 `async def _restore(self, target: str, chat_id: int, send: SendButtons) -> dict[str, Any]:`
    pub(crate) async fn restore(self: &Arc<Self>, target: &str, chat_id: i64, send: &SendButtons) -> anyhow::Result<Obj> {
        let store = self.app.store();
        let gone = store.trashed();
        let (app, candidates) = AppStore::pick(&gone, target);
        let Some(app) = app else {
            if !candidates.is_empty() {
                return Ok(self.missing(target, Some(&candidates)));
            }
            let names = gone.iter().take(8).map(|a| a.title.clone()).collect::<Vec<_>>().join(", ");
            let names = if names.is_empty() { "none".to_string() } else { names };
            return Ok(obj(json!({"ok": false, "reason": format!(
                "no deleted app called {}; the deleted apps are: {names}", eve_app_registry::py_str_repr(target))})));
        };
        let back = match store.restore(&app.slug) {
            Ok(back) => back,
            Err(StoreError::Exists(_)) => {
                return Ok(obj(json!({"ok": false, "reason": format!(
                    "a new app now has {}'s address, so it cannot come back under it; rename or delete the new one \
                     first", app.title)})));
            }
            Err(StoreError::Missing(_)) => {
                return Ok(obj(json!({"ok": false, "reason": format!("{} is no longer in the trash", app.title)})));
            }
            Err(e) => return Err(e.into()),
        };
        let url = self.app.app_url(&back.slug);
        if !url.is_empty() {
            let line = py_strip(&format!("{} {} is back, with its data.", back.icon, back.title)).to_string();
            send(chat_id, line, vec![(format!("Open {}", back.title), url)]).await?;
        }
        Ok(obj(json!({"ok": true, "restored": back.title, "sent": "an Open button is in the chat already"})))
    }

    /// py: apps_maker.py:1874 `def _progress(self, chat_id: int, say: Callable[[int, str], Awaitable[Any]]) -> Progress:`
    ///
    /// A short line when a chat build reaches the phone test and each repair round, never more often than
    /// CHAT_PROGRESS_SECS: a build runs for minutes, and silence reads as a build that died.
    pub(crate) fn progress(self: &Arc<Self>, chat_id: i64, say: SayFn) -> MakerProgress {
        struct State {
            stage: String,
            tested: bool,
            at: f64,
        }
        let gap = *self.progress_secs.lock();
        let state = Arc::new(Mutex::new(State { stage: String::new(), tested: false, at: -gap }));
        Arc::new(move |stage: &str, n: i64| {
            let line = {
                let mut s = state.lock();
                let before = std::mem::replace(&mut s.stage, stage.to_string());
                if stage == before || !matches!(stage, "testing" | "fixing") || (stage == "testing" && s.tested) {
                    return; // the first test, then each repair round; no more
                }
                s.tested = true;
                if monotonic() - s.at < gap {
                    return;
                }
                s.at = monotonic();
                if stage == "testing" {
                    "Written. Testing it on a phone…".to_string()
                } else {
                    format!(
                        "The phone test found {n} {}; fixing {}…",
                        if n == 1 { "problem" } else { "problems" },
                        if n == 1 { "it" } else { "them" }
                    )
                }
            };
            // no running loop: no line
            let Ok(rt) = tokio::runtime::Handle::try_current() else {
                return;
            };
            let task = rt.spawn(say(chat_id, line));
            rt.spawn(async move {
                if let Err(e) = task.await {
                    if !e.is_cancelled() {
                        tracing::info!("apps: a progress line did not reach the chat (JoinError)");
                    }
                }
            });
        })
    }

    /// py: apps_maker.py:1905 `async def _build(self, chat_id: int, request: str, target: str, key: str, send: SendButtons, say: Callable[[int, str], Awaitable[Any]], send_photo: Optional[SendPhoto]) -> None:`
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn build(self: &Arc<Self>, chat_id: i64, request: &str, target: &str, key: &str, send: SendButtons, say: SayFn, send_photo: Option<SendPhoto>) {
        let store = self.app.store();
        let info = if target.is_empty() { None } else { store.info(target) };
        // a change that fails leaves the app as it was, and the line says so
        let failed = if target.is_empty() {
            "I couldn't build that app".to_string()
        } else {
            format!("I couldn't change {}; it is unchanged", info.as_ref().map(|i| i.title.as_str()).unwrap_or(target))
        };
        let progress = self.progress(chat_id, say.clone());
        // a change that failed leads to the app's change chat too: its thread shows the try, and it is where
        // to ask again in other words
        let fail = |line: String| {
            let chat =
                if !target.is_empty() && store.info(target).is_some() { self.app.chat_url(target) } else { String::new() };
            let send = send.clone();
            let say = say.clone();
            async move {
                if chat.is_empty() {
                    say(chat_id, line).await;
                } else if let Err(e) = send(chat_id, line, vec![("Change".to_string(), chat)]).await {
                    tracing::warn!("apps: the failed build's line did not reach the chat: {e:#}");
                }
            }
        };
        let maker = self.app.maker();
        let made =
            if target.is_empty() { maker.make(request, Some(progress)).await } else { maker.edit(target, request, Some(progress)).await };
        if let Some(e) = &made.error {
            // a failed build is a line in the chat
            tracing::warn!("apps: building from the chat failed ({})", error_kind(e));
            fail(format!("{failed}: {}", crate::miniapp::error_line(e))).await;
            self.building.lock().remove(key);
            return;
        }
        self.building.lock().remove(key);
        let app = match (&made.app, made.ok) {
            (Some(app), true) => app.clone(),
            _ => {
                fail(format!("{failed}: {}.", made.reason)).await;
                return;
            }
        };
        let url = self.app.app_url(&app.slug);
        if url.is_empty() {
            say(chat_id, format!("{} is ready. Open it from the Apps tab on the menu button.", app.title)).await;
            return;
        }
        let mut caption =
            py_strip(&format!("{} {} is {}.", app.icon, app.title, if target.is_empty() { "ready" } else { "updated" }))
                .to_string();
        if !app.description.is_empty() {
            caption.push_str(&format!("\n{}", app.description));
        }
        if !made.issues.is_empty() {
            let first = owner_issues(made.check.as_ref(), &made.issues[..1], 240).into_iter().next().unwrap_or_default();
            caption.push_str(&format!(
                "\nIt may still have a problem: {first} {}",
                if target.is_empty() { "Tell me what to fix." } else { "Say \"undo\" to go back." }
            ));
        }
        let mut buttons = vec![(format!("Open {}", app.title), url)];
        if !target.is_empty() {
            let chat = self.app.chat_url(&app.slug);
            if !chat.is_empty() {
                buttons.push(("Change".to_string(), chat)); // the app's change chat, to keep refining
            }
        }
        let shot = made.check.as_ref().and_then(|c| c.screenshot.clone()).filter(|s| !s.is_empty());
        if let (Some(shot), Some(send_photo)) = (shot, &send_photo) {
            // the picture is the phone test's run: its sample entries, not the owner's data
            match send_photo(chat_id, shot, format!("{caption}\n(Picture: a test run with sample entries.)"), buttons.clone())
                .await
            {
                Ok(()) => return,
                // no picture is fine; the button still goes
                Err(e) => tracing::warn!("apps: sending the app's picture failed ({})", error_kind(&e)),
            }
        }
        if let Err(e) = send(chat_id, caption, buttons).await {
            tracing::warn!("apps: the finished app's buttons did not reach the chat: {e:#}");
        }
    }
}

/// A `json!({...})` object as an [`Obj`].
fn obj(v: Value) -> Obj {
    match v {
        Value::Object(m) => m,
        _ => Obj::new(),
    }
}

/// py: apps_maker.py:1152 `Progress = Callable[[str, int], None]`: (phase, characters written so far).
pub type MakerProgress = Arc<dyn Fn(&str, i64) + Send + Sync>;
/// py: apps_maker.py:1153 `Generate = Callable[[list[dict[str, Any]], Progress], Awaitable[Generated]]`; an error is the
/// call raising (stopped_reason reads it).
pub type Generate =
    Arc<dyn Fn(Vec<Value>, MakerProgress) -> futures::future::BoxFuture<'static, anyhow::Result<Generated>> + Send + Sync>;
/// py: apps_maker.py:1154 `Check = Callable[..., Awaitable[app_check.CheckReport]]`: (html, options) as
/// app_check.check_app takes them.
pub type Check = Arc<
    dyn Fn(String, eve_app_check::CheckOptions) -> futures::future::BoxFuture<'static, eve_app_check::CheckReport>
        + Send
        + Sync,
>;
/// `cost(usage) -> dollars`.
pub type CostFn = Arc<dyn Fn(&Value) -> f64 + Send + Sync>;
/// `spend(dollars)` (SpendLedger.add).
pub type SpendFn = Arc<dyn Fn(f64) -> f64 + Send + Sync>;
