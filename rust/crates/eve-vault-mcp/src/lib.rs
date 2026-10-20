//! The vault for Codex tasks: a stdio MCP server with two tools, `vault_list` and `vault_fill`.
//!
//! A Codex computer task (codex_computer) gets this server in its thread's config whenever the vault holds an
//! item. Codex calls `vault_fill` with an item's handle, the part it wants (`password`, `number`, `code`...)
//! and the page's URL; this server finds the field in that page through the Accessibility API and sets its value
//! (eve-vault-native: no key is pressed, no focus is needed, the browser need not be in front). The value goes
//! from the Keychain to that one field; the tool's answer says what was filled, never what. `vault_list` gives
//! the lines the browser lane's planner sees (`Item::line`).
//!
//! Which page: for a login or 2FA item, a page on the item's site (`site_allows`, against the URL the
//! Accessibility tree gives, never Codex's word); for a card, the page Codex names. Pages on other sites are
//! never walked into. Several such pages: the one Codex names, else the one with a focused field, else refused.
//!
//! Which field, by what it is (eve-vault-native's docs say why focus cannot decide it):
//!   password   the page's password field (AXSecureTextField); a password never goes anywhere else
//!   username   the field the page has focused, else the one called a username or email, else the last text
//!              field before the password field, else the page's only text field
//!   card parts, code   the focused field, else the one field whose label fits the part, else the only one
//! Codex may name the field's label (`field`) to narrow it. Anything still ambiguous is refused with the
//! candidates' labels, never guessed. Afterwards the field's character count is compared with the value's
//! length (a site that reformats a card number answers "it shows N characters", which is said, not hidden).
//!
//! Hand-written JSON-RPC 2.0 over newline-delimited stdio, the shape of eve_mcp. Nothing is logged: stdout is
//! the protocol, and nothing else is printed.

pub mod casefold;

use std::collections::HashSet;
use std::io::{self, BufRead, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use eve_vault::{parts, pystr, pyurl, site_allows, site_of, Vault, VaultError, ALL_PARTS, LINES_LIMIT};
use eve_vault_native::{Field, Native, NativeMac};
use parking_lot::Mutex;
use regex::Regex;
use serde_json::{json, Map, Value};

pub use casefold::casefold;

pub const PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
pub const SERVER_NAME: &str = "eve_vault";
pub const SERVER_VERSION: &str = "1.0.0";
pub const TOOL_NAMES: [&str; 2] = ["vault_list", "vault_fill"];
/// Seconds before each look for the page's fields.
pub const FIELD_WAITS: [f64; 5] = [0.0, 0.3, 0.5, 0.7, 1.0];
/// A frame's field reports its new length a moment after the set.
pub const COUNT_WAIT: f64 = 0.3;

/// `SERVER_INFO`: `{"name": "eve_vault", "version": "1.0.0"}`.
pub fn server_info() -> Value {
    json!({"name": SERVER_NAME, "version": SERVER_VERSION})
}

/// What a field is called, per part, to tell a page's fields apart (`Field::words`). Python's `re.I`.
pub static PART_WORDS: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    [
        ("number", r"card.?num|card number|cc.?num|\bpan\b"),
        ("cvc", r"cvc|cvv|csc|security code|card code"),
        ("exp", r"expir|exp.?date|mm.?/.?yy"),
        ("name", r"name on card|card.?holder"),
        ("code", r"code|otp|one.?time|verification|2fa|passcode"),
        ("username", r"user|e-?mail|login|account|phone"),
    ]
    .into_iter()
    .map(|(part, pattern)| (part, Regex::new(&format!("(?i){pattern}")).expect("PART_WORDS pattern")))
    .collect()
});

/// `PART_WORDS.get(part)`.
pub fn part_words(part: &str) -> Option<&'static Regex> {
    PART_WORDS.iter().find(|(p, _)| *p == part).map(|(_, r)| r)
}

pub const INSTRUCTIONS: &str = "eve's vault holds the owner's saved sign-ins, cards and 2FA secrets. vault_list shows them by id. \
To enter one, call vault_fill with the id, the part and the page's URL. You never see a value, \
and never type one yourself.";

/// The two tools' schemas, as `tools/list` returns them.
pub static TOOLS: LazyLock<Value> = LazyLock::new(|| {
    json!([
        {"name": "vault_list",
         "description": "The owner's saved sign-ins, cards and 2FA secrets: one line each with its id, kind, label \
and the site it is for. Never a value.",
         "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}},
        {"name": "vault_fill",
         "description": "Put one part of a saved item into a field of the page, without you seeing it. Give the \
page's URL, and the field's label when the page has several fields of that kind. Parts: a \
login has username and password; a card has number, exp (MM/YY), exp_month, exp_year, cvc and \
name; a 2FA item has code (generated now). A login or 2FA item fills only on its own site; a \
password fills only into a password field.",
         "inputSchema": {"type": "object", "additionalProperties": false, "required": ["id", "part"],
                         "properties": {"id": {"type": "string", "description": "The item's id from vault_list"},
                                        "part": {"type": "string", "enum": ALL_PARTS},
                                        "page": {"type": "string", "description": "The URL of the page the field is on"},
                                        "field": {"type": "string", "description": "The field's label, when needed"}}}},
    ])
});

/// A Python exception escaping a tool: `call` answers it as "The vault could not do that (<name>)." The name is
/// the Python class the same failure raises there, so the owner and Codex read the same sentence from both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Raised(pub String);

impl Raised {
    fn new(name: &str) -> Self {
        Raised(name.to_string())
    }

    /// What Python raises where the Rust vault returns `VaultError::Os` (a `security` run that timed out or
    /// could not start, an index that could not be written).
    fn from_os(message: &str) -> Self {
        let name = if message.starts_with("Command ") && message.contains(" timed out after ") {
            "TimeoutExpired"
        } else if message.contains("(os error 2)") {
            "FileNotFoundError"
        } else if message.contains("(os error 13)") || message.contains("(os error 1)") {
            "PermissionError"
        } else {
            "OSError"
        };
        Raised::new(name)
    }
}

/// The Mac, or a test's fake with the same four methods.
pub type SharedNative = Arc<dyn Native + Send + Sync>;
/// How the Mac is made on first use (Python's `native` property builds `NativeMac()` lazily).
pub type MakeNative = Box<dyn Fn() -> anyhow::Result<SharedNative> + Send + Sync>;

pub struct Server {
    pub vault: Arc<Vault>,
    native: Mutex<Option<SharedNative>>,
    make_native: MakeNative,
    out: Mutex<Box<dyn Write + Send>>,
    /// `FIELD_WAITS`, per server so a test can shorten them.
    pub field_waits: Vec<f64>,
    /// `COUNT_WAIT`, per server so a test can shorten it.
    pub count_wait: f64,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server").field("vault", &self.vault).finish()
    }
}

fn sleep(seconds: f64) {
    if seconds > 0.0 {
        std::thread::sleep(Duration::from_secs_f64(seconds));
    }
}

fn make_native_mac() -> anyhow::Result<SharedNative> {
    Ok(Arc::new(NativeMac::new()?))
}

impl Server {
    /// `Server(vault, native, out)`: the native defaults to `NativeMac`, made on first use; `out` is where the
    /// replies go (stdout for the real server).
    pub fn new(vault: Arc<Vault>, native: Option<SharedNative>, out: Box<dyn Write + Send>) -> Self {
        Server {
            vault,
            native: Mutex::new(native),
            make_native: Box::new(make_native_mac),
            out: Mutex::new(out),
            field_waits: FIELD_WAITS.to_vec(),
            count_wait: COUNT_WAIT,
        }
    }

    /// `Server()`: the default vault (`EVE_VAULT_INDEX`, `EVE_VAULT_KEYCHAIN`), the real Mac, stdout.
    pub fn open_default() -> eve_vault::Result<Self> {
        Ok(Server::new(Arc::new(Vault::open_default()?), None, Box::new(io::stdout())))
    }

    /// Replaces how the Mac is made on first use.
    pub fn with_make_native(mut self, make: MakeNative) -> Self {
        self.make_native = make;
        self
    }

    /// The Mac, made on first use. Python's `NativeMac()` raises `RuntimeError` off macOS; it is retried on the
    /// next call, as Python leaves `_native` unset.
    pub fn native(&self) -> Result<SharedNative, Raised> {
        let mut native = self.native.lock();
        if let Some(n) = native.as_ref() {
            return Ok(n.clone());
        }
        let made = (self.make_native)().map_err(|_| Raised::new("RuntimeError"))?;
        *native = Some(made.clone());
        Ok(made)
    }

    // -- the tools --

    pub fn vault_list(&self) -> (bool, String) {
        let lines = self.vault.lines(LINES_LIMIT);
        (true, if lines.is_empty() { "The vault is empty.".to_string() } else { lines.join("\n") })
    }

    /// `vault_fill(handle, part, page, label)` with the tool's raw arguments (any JSON, as Codex sent them).
    pub fn vault_fill(
        &self,
        handle: Option<&Value>,
        part: Option<&Value>,
        page: Option<&Value>,
        label: Option<&Value>,
    ) -> Result<(bool, String), Raised> {
        let handle = pystr::strip(&pystr::str_or_empty(handle)).to_string();
        let part = pystr::strip(&pystr::str_or_empty(part)).to_string();
        let page = pystr::strip(&pystr::str_or_empty(page)).to_string();
        let label = pystr::squash(&pystr::str_or_empty(label));
        let Some(item) = self.vault.get(&handle) else {
            return Ok((false, format!("No vault item {}. vault_list shows the ids.", pystr::repr(&pystr::head(&handle, 20)))));
        };
        let kind_parts = parts(&item.kind);
        if !kind_parts.contains(&part.as_str()) {
            return Ok((false, format!("A {} has the parts {}.", item.kind, kind_parts.join(", "))));
        }
        if item.origin.is_empty() && page.is_empty() {
            return Ok((false, format!("Give the URL of the page the {part} goes into.")));
        }
        let native = self.native()?;
        if !native.trusted() {
            return Ok((false, "eve has no Accessibility permission here, so it cannot reach the page.".to_string()));
        }

        // `_same_page` raises ValueError on a malformed URL; inside `want` that escapes the walk in Python.
        let raised: Mutex<Option<Raised>> = Mutex::new(None);
        let want = |url: &str| -> bool {
            if !item.origin.is_empty() {
                return site_allows(&item.origin, url);
            }
            match same_page(url, &page) {
                Ok(same) => same,
                Err(e) => {
                    raised.lock().get_or_insert(e);
                    false
                }
            }
        };
        let mut fields: Vec<Field> = vec![];
        for &wait in &self.field_waits {
            // Chrome builds a page's Accessibility tree after a client first asks (live, 2026-10-12: a page
            // just loaded showed no fields on the first walk and all of them a second later)
            sleep(wait);
            fields = native.page_fields(&want, item.kind == "card");
            if let Some(e) = raised.lock().take() {
                return Err(e);
            }
            if !fields.is_empty() {
                break;
            }
        }
        let where_item = if item.origin.is_empty() { page.clone() } else { site_of(&item.origin) };
        if fields.is_empty() {
            return Ok((
                false,
                format!(
                    "No page on {where_item} with a field is in front in its window, so the vault cannot see \
it (it reads each window's active tab). Show that tab, then call vault_fill again."
                ),
            ));
        }
        // a frame's fields belong to its page
        let top = |f: &Field| (f.pid, if f.page.is_empty() { f.url.clone() } else { f.page.clone() });
        let mut pages: HashSet<(i32, String)> = fields.iter().map(top).collect();
        if pages.len() > 1 && !page.is_empty() {
            let mut named = vec![];
            for f in &fields {
                if same_page(if f.page.is_empty() { &f.url } else { &f.page }, &page)? {
                    named.push(f.clone());
                }
            }
            if !named.is_empty() {
                fields = named;
            }
            pages = fields.iter().map(top).collect();
        }
        if pages.len() > 1 {
            let hot: HashSet<(i32, String)> = fields.iter().filter(|f| f.focused).map(top).collect();
            if hot.len() != 1 {
                return Ok((false, format!("{} pages on {where_item} are open. Give the page's exact URL.", pages.len())));
            }
            fields.retain(|f| hot.contains(&top(f)));
        }
        let field = match pick_field(&fields, &part, &label) {
            Ok(field) => field,
            Err(why) => return Ok((false, why)),
        };
        let (item, mut values) = match self.vault.fill_values(&handle, &field.url, &part) {
            Ok(got) => got,
            Err(VaultError::Os(message)) => return Err(Raised::from_os(&message)),
            Err(e) => return Ok((false, e.to_string())),
        };
        let Some(value) = values.shift_remove(&part) else {
            return Err(Raised::new("KeyError"));
        };
        drop(values);
        let ok = native.set_value(&field, &value);
        let want_n = value.chars().count() as i64;
        let mut n = native.char_count(&field);
        if n.is_some_and(|n| n != want_n) {
            sleep(self.count_wait); // live 2026-10-12: a payment frame's field said 0 right after the set
            n = native.char_count(&field);
        }
        drop(value);
        let place = if field.url.contains("://") { site_of(&field.url) } else { field.url.clone() };
        if !ok {
            return Ok((false, format!("The {part} field on {place} would not take a value. Nothing was filled.")));
        }
        if let Some(n) = n.filter(|&n| n != want_n) {
            return Ok((
                true,
                format!(
                    "Put the {part} of {} into the field on {place}, but it shows {n} characters \
where {want_n} were set (the site may format it). Check the page.",
                    pystr::repr(&item.label)
                ),
            ));
        }
        let kind = if field.secure() {
            "password".to_string()
        } else if !field.words.is_empty() {
            pystr::repr(&pystr::head(&field.words, 40))
        } else {
            "text".to_string()
        };
        Ok((true, format!("Filled the {part} of {} into the {kind} field on {place}.", pystr::repr(&item.label))))
    }

    /// `tools/call`: the tool's answer as MCP content. Codex gets a sentence, never a dead server or a traceback.
    pub fn call(&self, name: Option<&Value>, args: Option<&Value>) -> Value {
        let empty = Map::new();
        let args = args.and_then(Value::as_object).unwrap_or(&empty);
        let run = || -> Result<(bool, String), Raised> {
            match name.and_then(Value::as_str) {
                Some("vault_list") => Ok(self.vault_list()),
                Some("vault_fill") => self.vault_fill(args.get("id"), args.get("part"), args.get("page"), args.get("field")),
                _ => Ok((
                    false,
                    format!("No tool {}.", pystr::repr(&pystr::head(&pystr::value_str(name.unwrap_or(&Value::Null)), 40))),
                )),
            }
        };
        let (ok, text) = match catch_unwind(AssertUnwindSafe(run)) {
            Ok(Ok(answer)) => answer,
            Ok(Err(Raised(kind))) => (false, format!("The vault could not do that ({kind}).")),
            Err(_) => (false, "The vault could not do that (Exception).".to_string()),
        };
        json!({"content": [{"type": "text", "text": text}], "isError": !ok})
    }

    // -- the wire --

    /// One reply line: `json.dumps(msg, ensure_ascii=False)` and a newline, flushed.
    pub fn write(&self, msg: &Value) -> io::Result<()> {
        let mut line = dumps(msg);
        line.push('\n');
        let mut out = self.out.lock();
        out.write_all(line.as_bytes())?;
        out.flush()
    }

    pub fn handle(&self, msg: &Value) -> io::Result<()> {
        let obj = msg.as_object();
        let valid = obj.is_some_and(|o| o.get("jsonrpc") == Some(&json!("2.0")) && o.get("method").is_some_and(Value::is_string));
        let Some(obj) = obj.filter(|_| valid) else {
            if let Some(o) = obj {
                if o.contains_key("id") && !o.contains_key("method") {
                    return Ok(()); // a response to nothing we asked: no answer
                }
            }
            let id = obj.and_then(|o| o.get("id")).cloned().unwrap_or(Value::Null);
            return self.write(&json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32600, "message": "invalid request"}}));
        };
        let Some(rid) = obj.get("id") else {
            return Ok(()); // a notification: nothing to answer
        };
        let method = obj.get("method").and_then(Value::as_str).unwrap_or_default();
        let empty = Value::Object(Map::new());
        let params = obj.get("params").filter(|p| p.is_object()).unwrap_or(&empty);
        let result = match method {
            "initialize" => {
                let asked = params.get("protocolVersion").and_then(Value::as_str);
                let version = asked.filter(|a| PROTOCOL_VERSIONS.contains(a)).unwrap_or(PROTOCOL_VERSIONS[0]);
                json!({"protocolVersion": version,
                       "capabilities": {"tools": {"listChanged": false}},
                       "serverInfo": server_info(), "instructions": INSTRUCTIONS})
            }
            "ping" => json!({}),
            "tools/list" => json!({"tools": *TOOLS}),
            "tools/call" => self.call(params.get("name"), params.get("arguments")),
            _ => {
                return self.write(&json!({"jsonrpc": "2.0", "id": rid,
                                          "error": {"code": -32601, "message": format!("method not found: {method}")}}));
            }
        };
        self.write(&json!({"jsonrpc": "2.0", "id": rid, "result": result}))
    }

    pub fn handle_line(&self, raw: &str) -> io::Result<()> {
        let raw = pystr::strip(raw);
        if raw.is_empty() {
            return Ok(());
        }
        let Ok(msg) = serde_json::from_str::<Value>(raw) else {
            return self.write(&json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "parse error"}}));
        };
        match &msg {
            Value::Array(batch) => {
                for one in batch {
                    self.handle(one)?;
                }
                Ok(())
            }
            one => self.handle(one),
        }
    }

    /// Reads requests a line at a time until stdin closes. Lines split as Python's text-mode stdin splits them
    /// (universal newlines: `\n`, `\r\n` and a lone `\r`).
    pub fn serve<R: BufRead>(&self, mut input: R) -> io::Result<i32> {
        let mut buf = Vec::new();
        loop {
            buf.clear();
            if input.read_until(b'\n', &mut buf)? == 0 {
                return Ok(0);
            }
            let text = String::from_utf8_lossy(&buf);
            let text = text.replace("\r\n", "\n").replace('\r', "\n");
            for line in text.split_inclusive('\n') {
                self.handle_line(line)?;
            }
        }
    }
}

/// The one field of one page that `part` goes into, or why there is none. See the module docs.
pub fn pick_field(fields: &[Field], part: &str, label: &str) -> Result<Field, String> {
    let mut fields: Vec<Field> = fields.to_vec();
    fields.sort_by_key(|f| f.order);
    if !label.is_empty() {
        let want = casefold(label);
        let named: Vec<Field> = fields.iter().filter(|f| casefold(&f.words).contains(&want)).cloned().collect();
        if named.is_empty() {
            return Err(format!("No field labelled {} on the page.", pystr::repr(label)));
        }
        fields = named;
    }
    let secure: Vec<&Field> = fields.iter().filter(|f| f.secure()).collect();
    if part == "password" {
        let Some(first) = secure.first() else {
            return Err(format!(
                "A password goes only into a password field, and the page has none{}",
                if label.is_empty() { "." } else { " with that label." }
            ));
        };
        return Ok((*secure.iter().find(|f| f.focused).unwrap_or(first)).clone());
    }
    let plain: Vec<&Field> = fields.iter().filter(|f| !f.secure()).collect();
    if plain.is_empty() {
        return Err(format!("The page has no field for the {part}."));
    }
    let focused: Vec<&&Field> = plain.iter().filter(|f| f.focused).collect();
    if focused.len() == 1 {
        return Ok((**focused[0]).clone());
    }
    let words = part_words(part);
    let named: Vec<&Field> = plain.iter().copied().filter(|f| words.is_some_and(|w| w.is_match(&f.words))).collect();
    if named.len() == 1 {
        return Ok(named[0].clone());
    }
    if part == "username" {
        if let Some(first_secure) = secure.first() {
            if let Some(before) = plain.iter().rfind(|f| f.order < first_secure.order) {
                return Ok((*before).clone());
            }
        }
    }
    if plain.len() == 1 {
        return Ok(plain[0].clone());
    }
    let shown = if named.is_empty() { &plain } else { &named };
    let labels: Vec<String> = shown
        .iter()
        .take(6)
        .map(|f| {
            let head = pystr::head(&f.words, 30);
            pystr::repr(if head.is_empty() { "unlabelled" } else { &head })
        })
        .collect();
    Err(format!("The page has several fields the {part} could go into ({}). Give the field's label.", labels.join(", ")))
}

/// `_same_page`: the field's page is the one Codex named: the same origin and path (a fragment or query aside).
/// A page named without a scheme ("shop.com/checkout") is read as https, as `canonical_origin` reads a site.
/// A URL `urlsplit` refuses raises `ValueError`, as in Python.
pub fn same_page(url: &str, page: &str) -> Result<bool, Raised> {
    fn split(u: &str) -> Result<(String, String, String), Raised> {
        let u = pystr::strip(u);
        let full = if u.contains("://") { u.to_string() } else { format!("https://{u}") };
        let s = pyurl::urlsplit(&full).map_err(|_| Raised::new("ValueError"))?;
        Ok((s.scheme, s.netloc.to_lowercase(), s.path.trim_end_matches('/').to_string()))
    }
    Ok(split(url)? == split(page)?)
}

/// The vault's own two settings, which travel with the server because Codex starts it with a trimmed
/// environment: `EVE_VAULT_INDEX` and `EVE_VAULT_KEYCHAIN`, each only when set and non-empty.
pub fn vault_env(lookup: &dyn Fn(&str) -> Option<String>) -> Map<String, Value> {
    let mut env = Map::new();
    for k in ["EVE_VAULT_INDEX", "EVE_VAULT_KEYCHAIN"] {
        if let Some(v) = lookup(k).filter(|v| !v.is_empty()) {
            env.insert(k.to_string(), Value::from(v));
        }
    }
    env
}

fn with_env(mut config: Map<String, Value>, env: Map<String, Value>) -> Value {
    if !env.is_empty() {
        config.insert("env".into(), Value::Object(env));
    }
    Value::Object(config)
}

fn process_env(k: &str) -> Option<String> {
    std::env::var(k).ok()
}

/// The `eve-vault-mcp` binary beside the running one (the Rust daemon and this server are built together),
/// else the bare name for `PATH` to find.
pub fn default_command() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join("eve-vault-mcp")))
        .filter(|p: &PathBuf| p.exists())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "eve-vault-mcp".to_string())
}

/// The `mcp_servers` entry a Codex thread is started with (codex_computer), starting this crate's binary:
/// `command` (default [`default_command`]), no arguments, and the vault's settings from the process env.
pub fn server_config(command: &str) -> Value {
    server_config_with(command, &process_env)
}

/// [`server_config`] over an explicit environment.
pub fn server_config_with(command: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Value {
    let command = if command.is_empty() { default_command() } else { command.to_string() };
    let mut config = Map::new();
    config.insert("command".into(), Value::from(command));
    config.insert("args".into(), json!([]));
    with_env(config, vault_env(lookup))
}

/// Python's `server_config(python)` exactly: the Python daemon's server, `<python> -I -m eve_bridge.vault_mcp`.
/// `python` empty is the `python3` on `PATH` (Python uses its own `sys.executable`).
pub fn python_server_config(python: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Value {
    let mut config = Map::new();
    config.insert("command".into(), Value::from(if python.is_empty() { "python3" } else { python }));
    config.insert("args".into(), json!(["-I", "-m", "eve_bridge.vault_mcp"]));
    with_env(config, vault_env(lookup))
}

/// `main()`: serve stdin with the default server. A server that cannot be made (a bad `EVE_VAULT_KEYCHAIN`)
/// says why on stderr and exits 1, where Python dies with a traceback.
pub fn main() -> i32 {
    main_with(|| Server::open_default().map_err(|e| e.to_string()))
}

/// `main(make=...)`: serve stdin with the server `make` builds.
pub fn main_with(make: impl FnOnce() -> Result<Server, String>) -> i32 {
    let server = match make() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("eve_vault: {e}");
            return 1;
        }
    };
    match server.serve(io::stdin().lock()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("eve_vault: {e}");
            1
        }
    }
}

/// `json.dumps(value, ensure_ascii=False)`: `", "` and `": "` separators, Python's float repr.
pub fn dumps(value: &Value) -> String {
    use serde::Serialize;
    let mut out = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut out, PyFormatter);
    value.serialize(&mut ser).expect("a JSON value always serializes");
    String::from_utf8(out).expect("serde_json writes UTF-8")
}

struct PyFormatter;

impl serde_json::ser::Formatter for PyFormatter {
    fn begin_array_value<W: ?Sized + Write>(&mut self, writer: &mut W, first: bool) -> io::Result<()> {
        if first {
            Ok(())
        } else {
            writer.write_all(b", ")
        }
    }

    fn begin_object_key<W: ?Sized + Write>(&mut self, writer: &mut W, first: bool) -> io::Result<()> {
        if first {
            Ok(())
        } else {
            writer.write_all(b", ")
        }
    }

    fn begin_object_value<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        writer.write_all(b": ")
    }

    fn write_f64<W: ?Sized + Write>(&mut self, writer: &mut W, value: f64) -> io::Result<()> {
        writer.write_all(pystr::float_repr(value).as_bytes())
    }

    fn write_f32<W: ?Sized + Write>(&mut self, writer: &mut W, value: f32) -> io::Result<()> {
        self.write_f64(writer, value as f64)
    }
}

#[cfg(test)]
mod tests;
