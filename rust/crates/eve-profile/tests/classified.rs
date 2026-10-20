//! profile::LITE_CLASSIFIED: every EVE_* switch a module of eve lite's daemon reads has a recorded reason to be
//! forced or left alone, and every secret those modules read is removed by profile::apply (port of
//! test_profile_classified.py).
//!
//! The module set is worked out here, from the Python source both daemons share: the lite composition (lite.py and
//! the parts it builds, M1) and everything their imports reach, function-level imports included (a lazy import runs
//! the day its function does), plus what importing the CLI loads. Python's test reads the modules with `ast`; this
//! one reads them with a small Python tokenizer of its own (string literals, f-strings, imports, and which `def` an
//! import sits in), which is all the scan needs. A new switch, or a new secret, in any of those modules fails here
//! until someone decides what eve lite does with it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use eve_profile as profile;
use regex::Regex;

const CANARY: &str = "CANARY-NOT-A-KEY-7f3c";
const PKG: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/src/eve_bridge");

/// The lite composition (M1): what lite.py builds, and lite's own modules, each counted once it exists.
const COMPOSITION: &[&str] = &[
    "telegram", "link_chat", "phone_call", "watch", "memory", "chat_log", "recall", "think", "spend_sync", "ipc",
    "envfile", "records", "dream", "profile", "toolchain",
];
const LITE_MODULES: &[&str] = &[
    "lite", "lite_web", "lite_rtc", "lite_tls", "lan_net", "web_auth", "onboarding", "composio_connect",
    "composio_login", "composio_platform_lite",
];
// Python's `$` without MULTILINE also matches before one trailing newline.
static SWITCH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^EVE_[A-Z0-9_*]+\n?$").unwrap());
static ENV_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Z][A-Z0-9_]*[A-Z0-9]\n?$").unwrap());
/// A name that mentions one of these may hold a secret: apply removes it (profile::is_secret_name), or
/// profile::NOT_SECRET_NAMES says what it holds instead.
static SECRETISH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"KEY|TOKEN|SECRET|PASSW|CREDENTIAL|AUTH|COOKIE|PRIVATE").unwrap());
static REASON_SWITCH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"EVE_[A-Z0-9_]+").unwrap());

// ---- a Python tokenizer, as far as the scan needs one ---------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Name(String),
    /// A string literal: its text (escapes decoded), whether it is bytes, and for an f-string its pieces.
    Str { value: String, bytes: bool, fpieces: Option<Vec<Piece>> },
    Op(char),
    Number,
}

#[derive(Debug, Clone, PartialEq)]
enum Piece {
    Lit(String),
    Expr(String),
}

#[derive(Debug, Default)]
struct Line {
    indent: usize,
    toks: Vec<Tok>,
}

fn decode_escapes(raw: &str) -> String {
    let mut out = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some('\'') => out.push('\''),
            Some('"') => out.push('"'),
            Some('0') => out.push('\0'),
            Some('\n') => {}
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// An f-string's body split into literal text and `{expression}` pieces (`{{` and `}}` are literal braces).
fn fstring_pieces(body: &str, raw: bool) -> Vec<Piece> {
    let chars: Vec<char> = body.chars().collect();
    let mut pieces = Vec::new();
    let mut lit = String::new();
    let mut i = 0;
    let flush = |lit: &mut String, pieces: &mut Vec<Piece>| {
        if !lit.is_empty() {
            let text = if raw { lit.clone() } else { decode_escapes(lit) };
            pieces.push(Piece::Lit(text));
            lit.clear();
        }
    };
    while i < chars.len() {
        let c = chars[i];
        if c == '{' && chars.get(i + 1) == Some(&'{') {
            lit.push('{');
            i += 2;
        } else if c == '}' && chars.get(i + 1) == Some(&'}') {
            lit.push('}');
            i += 2;
        } else if c == '{' {
            flush(&mut lit, &mut pieces);
            let mut depth = 0usize;
            let mut j = i + 1;
            let mut quote: Option<char> = None;
            while j < chars.len() {
                let d = chars[j];
                if let Some(q) = quote {
                    if d == q {
                        quote = None;
                    }
                } else if d == '\'' || d == '"' {
                    quote = Some(d);
                } else if "([{".contains(d) {
                    depth += 1;
                } else if ")]}".contains(d) {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
                j += 1;
            }
            pieces.push(Piece::Expr(chars[i + 1..j.min(chars.len())].iter().collect()));
            i = j + 1;
        } else {
            lit.push(c);
            i += 1;
        }
    }
    flush(&mut lit, &mut pieces);
    pieces
}

/// The logical lines of a Python source, each with its indentation and tokens.
fn tokenize(src: &str) -> Vec<Line> {
    let chars: Vec<char> = src.chars().collect();
    let mut lines: Vec<Line> = Vec::new();
    let mut cur = Line::default();
    let mut depth = 0usize;
    let mut at_line_start = true;
    let mut i = 0;
    let n = chars.len();
    let end_line = |cur: &mut Line, lines: &mut Vec<Line>| {
        if !cur.toks.is_empty() {
            lines.push(std::mem::take(cur));
        }
        *cur = Line::default();
    };
    while i < n {
        if at_line_start && depth == 0 {
            let mut col = 0;
            while i < n && (chars[i] == ' ' || chars[i] == '\t' || chars[i] == '\x0c') {
                col = if chars[i] == '\t' { (col / 8 + 1) * 8 } else { col + 1 };
                i += 1;
            }
            cur.indent = col;
            at_line_start = false;
            continue;
        }
        let c = chars[i];
        match c {
            '#' => {
                while i < n && chars[i] != '\n' {
                    i += 1;
                }
            }
            '\\' if chars.get(i + 1) == Some(&'\n') => i += 2,
            '\n' => {
                i += 1;
                if depth == 0 {
                    end_line(&mut cur, &mut lines);
                    at_line_start = true;
                }
            }
            ' ' | '\t' | '\r' | '\x0c' => i += 1,
            '\'' | '"' => {
                let (tok, next) = read_string(&chars, i, "");
                cur.toks.push(tok);
                i = next;
            }
            c if c.is_alphabetic() || c == '_' => {
                let start = i;
                while i < n && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                let lower = word.to_lowercase();
                if i < n
                    && (chars[i] == '\'' || chars[i] == '"')
                    && ["r", "u", "b", "f", "br", "rb", "fr", "rf"].contains(&lower.as_str())
                {
                    let (tok, next) = read_string(&chars, i, &lower);
                    cur.toks.push(tok);
                    i = next;
                } else {
                    cur.toks.push(Tok::Name(word));
                }
            }
            c if c.is_ascii_digit() => {
                while i < n && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '.') {
                    i += 1;
                }
                cur.toks.push(Tok::Number);
            }
            _ => {
                if "([{".contains(c) {
                    depth += 1;
                } else if ")]}".contains(c) {
                    depth = depth.saturating_sub(1);
                }
                cur.toks.push(Tok::Op(c));
                i += 1;
            }
        }
    }
    end_line(&mut cur, &mut lines);
    lines
}

/// One string literal starting at the quote `start`, with its prefix: the token and where the source goes on.
fn read_string(chars: &[char], start: usize, prefix: &str) -> (Tok, usize) {
    let q = chars[start];
    let triple = chars.get(start + 1) == Some(&q) && chars.get(start + 2) == Some(&q);
    let open = if triple { 3 } else { 1 };
    let mut i = start + open;
    let body_start = i;
    let body_end;
    loop {
        if i >= chars.len() {
            body_end = chars.len();
            break;
        }
        if chars[i] == '\\' {
            i += 2;
            continue;
        }
        if chars[i] == q && (!triple || (chars.get(i + 1) == Some(&q) && chars.get(i + 2) == Some(&q))) {
            body_end = i;
            i += open;
            break;
        }
        i += 1;
    }
    let body: String = chars[body_start..body_end.min(chars.len())].iter().collect();
    let raw = prefix.contains('r');
    let bytes = prefix.contains('b');
    let fpieces = prefix.contains('f').then(|| fstring_pieces(&body, raw));
    let value = if raw { body } else { decode_escapes(&body) };
    (Tok::Str { value, bytes, fpieces }, i)
}

/// What the scan reads from a string node: Python's ast.Constant (a str) or ast.JoinedStr (an f-string), after
/// adjacent literals are joined.
#[derive(Debug)]
enum StrNode {
    Constant(String),
    Joined(Vec<Piece>),
}

/// Every string node of a source, ast.walk's way: implicitly joined literals as one node, each f-string's literal
/// chunks as constants of their own, and the strings inside its expressions.
fn string_nodes(src: &str) -> Vec<StrNode> {
    let mut out = Vec::new();
    for line in tokenize(src) {
        line_string_nodes(&line.toks, &mut out);
    }
    out
}

fn line_string_nodes(toks: &[Tok], out: &mut Vec<StrNode>) {
    let mut i = 0;
    while i < toks.len() {
        if !matches!(toks[i], Tok::Str { .. }) {
            i += 1;
            continue;
        }
        let mut run = Vec::new();
        while let Some(Tok::Str { value, bytes, fpieces }) = toks.get(i) {
            run.push((value.clone(), *bytes, fpieces.clone()));
            i += 1;
        }
        if run.iter().any(|(_, bytes, _)| *bytes) {
            continue; // bytes, not str
        }
        if run.iter().any(|(_, _, f)| f.is_some()) {
            let mut pieces: Vec<Piece> = Vec::new();
            for (value, _, f) in run {
                let parts = f.unwrap_or_else(|| vec![Piece::Lit(value)]);
                for part in parts {
                    match (pieces.last_mut(), part) {
                        (Some(Piece::Lit(prev)), Piece::Lit(text)) => prev.push_str(&text),
                        (_, part) => pieces.push(part),
                    }
                }
            }
            for piece in &pieces {
                match piece {
                    Piece::Lit(text) => out.push(StrNode::Constant(text.clone())),
                    Piece::Expr(expr) => {
                        for line in tokenize(expr) {
                            line_string_nodes(&line.toks, out);
                        }
                    }
                }
            }
            out.push(StrNode::Joined(pieces));
        } else {
            out.push(StrNode::Constant(run.into_iter().map(|(v, _, _)| v).collect()));
        }
    }
}

/// An f-string's text with each value written as *: f"EVE_{name.upper()}_FILE" is EVE_*_FILE.
fn pattern(pieces: &[Piece]) -> String {
    pieces.iter().map(|p| match p {
        Piece::Lit(t) => t.as_str(),
        Piece::Expr(_) => "*",
    }).collect()
}

/// Every EVE_* name in the source: a string that is one, or an f-string that builds one.
fn switches_in(src: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for node in string_nodes(src) {
        match node {
            StrNode::Constant(v) if SWITCH.is_match(&v) => {
                found.insert(v);
            }
            StrNode::Joined(pieces) => {
                let p = pattern(&pieces);
                if SWITCH.is_match(&p) && p.starts_with("EVE_") {
                    found.insert(p);
                }
            }
            _ => {}
        }
    }
    found
}

/// Every string in the source shaped like an environment variable's name.
fn names_in(src: &str) -> BTreeSet<String> {
    string_nodes(src)
        .into_iter()
        .filter_map(|n| match n {
            StrNode::Constant(v) if ENV_NAME.is_match(&v) => Some(v),
            _ => None,
        })
        .collect()
}

// ---- the module set --------------------------------------------------------------------------------------------------

fn all_modules() -> BTreeMap<String, PathBuf> {
    let mut found = BTreeMap::new();
    for entry in walk_py(Path::new(PKG)) {
        let rel = entry.strip_prefix(PKG).unwrap().with_extension("");
        let mut parts: Vec<String> = rel.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        if parts.last().map(String::as_str) == Some("__init__") {
            parts.pop();
        }
        let mut name = vec!["eve_bridge".to_string()];
        name.extend(parts);
        found.insert(name.join("."), entry);
    }
    found
}

fn walk_py(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            out.extend(walk_py(&path));
        } else if path.extension().and_then(|e| e.to_str()) == Some("py") {
            out.push(path);
        }
    }
    out
}

static SOURCES: LazyLock<std::sync::Mutex<HashMap<PathBuf, String>>> = LazyLock::new(Default::default);

fn source(path: &Path) -> String {
    let mut cache = SOURCES.lock().unwrap();
    cache.entry(path.to_path_buf()).or_insert_with(|| std::fs::read_to_string(path).unwrap()).clone()
}

/// A dotted name at `toks[i..]`: the name and where the tokens go on.
fn dotted(toks: &[Tok], mut i: usize) -> (String, usize) {
    let mut parts = Vec::new();
    while let Some(Tok::Name(n)) = toks.get(i) {
        parts.push(n.clone());
        i += 1;
        if toks.get(i) == Some(&Tok::Op('.')) && matches!(toks.get(i + 1), Some(Tok::Name(_))) {
            i += 1;
        } else {
            break;
        }
    }
    (parts.join("."), i)
}

/// The names a statement imports, Python's ast.Import / ast.ImportFrom read the test's way: the module, and each
/// name it imports from it.
fn statement_imports(toks: &[Tok], package: &[String], wanted: &mut BTreeSet<String>) {
    match toks.first() {
        Some(Tok::Name(kw)) if kw == "import" => {
            let mut i = 1;
            loop {
                let (name, next) = dotted(toks, i);
                if !name.is_empty() {
                    wanted.insert(name);
                }
                i = next;
                if matches!(toks.get(i), Some(Tok::Name(a)) if a == "as") {
                    i += 2;
                }
                if toks.get(i) == Some(&Tok::Op(',')) {
                    i += 1;
                } else {
                    break;
                }
            }
        }
        Some(Tok::Name(kw)) if kw == "from" => {
            let mut i = 1;
            let mut level = 0;
            while toks.get(i) == Some(&Tok::Op('.')) {
                level += 1;
                i += 1;
            }
            let (module, next) =
                if matches!(toks.get(i), Some(Tok::Name(k)) if k == "import") { (String::new(), i) } else { dotted(toks, i) };
            i = next;
            if !matches!(toks.get(i), Some(Tok::Name(k)) if k == "import") {
                return;
            }
            i += 1;
            let base = if level > 0 {
                let keep = (package.len() + 1).saturating_sub(level).min(package.len());
                let mut parts: Vec<String> = package[..keep].to_vec();
                if !module.is_empty() {
                    parts.push(module);
                }
                parts.join(".")
            } else {
                module
            };
            wanted.insert(base.clone());
            while i < toks.len() {
                match &toks[i] {
                    Tok::Name(n) if n == "as" => i += 2,
                    Tok::Name(n) => {
                        wanted.insert(format!("{base}.{n}"));
                        i += 1;
                    }
                    Tok::Op('*') => {
                        wanted.insert(format!("{base}.*"));
                        i += 1;
                    }
                    _ => i += 1,
                }
            }
        }
        _ => {}
    }
}

/// The eve_bridge modules `name` imports: at module level only, or anywhere in it (functions included). A submodule
/// brings its packages' __init__ along; importlib.import_module with a fixed name counts too.
fn imports_of(name: &str, path: &Path, modules: &BTreeMap<String, PathBuf>, top_only: bool) -> BTreeSet<String> {
    let parts: Vec<String> = name.split('.').map(str::to_string).collect();
    let package: Vec<String> =
        if path.file_name().unwrap() == "__init__.py" { parts } else { parts[..parts.len() - 1].to_vec() };
    let mut wanted = BTreeSet::new();
    // (indent, is a function) for each def and class the line is inside
    let mut blocks: Vec<(usize, bool)> = Vec::new();
    for line in tokenize(&source(path)) {
        while blocks.last().is_some_and(|(indent, _)| *indent >= line.indent) {
            blocks.pop();
        }
        let toks = &line.toks;
        let starts_def = matches!(toks.first(), Some(Tok::Name(k)) if k == "def")
            || (matches!(toks.first(), Some(Tok::Name(k)) if k == "async")
                && matches!(toks.get(1), Some(Tok::Name(k)) if k == "def"));
        let starts_class = matches!(toks.first(), Some(Tok::Name(k)) if k == "class");
        let mut inside = blocks.iter().any(|(_, f)| *f) || starts_def;
        if starts_def || starts_class {
            blocks.push((line.indent, starts_def));
        }
        if !(top_only && inside) {
            statement_imports(toks, &package, &mut wanted);
            // a statement after a colon on the same line (`if x: import y`, `try: import y`)
            for (j, tok) in toks.iter().enumerate() {
                if *tok == Tok::Op(':') && matches!(toks.get(j + 1), Some(Tok::Name(k)) if k == "import" || k == "from") {
                    statement_imports(&toks[j + 1..], &package, &mut wanted);
                }
            }
        }
        for (j, tok) in toks.iter().enumerate() {
            if matches!(tok, Tok::Name(k) if k == "lambda") {
                inside = true;
            }
            if top_only && inside {
                continue;
            }
            if matches!(tok, Tok::Name(k) if k == "import_module")
                && j > 0
                && toks[j - 1] == Tok::Op('.')
                && toks.get(j + 1) == Some(&Tok::Op('('))
            {
                if let Some(Tok::Str { value, bytes: false, fpieces: None }) = toks.get(j + 2) {
                    wanted.insert(value.clone());
                }
            }
        }
    }
    let mut found = BTreeSet::new();
    for module in wanted {
        let mut module = module;
        while !module.is_empty() && !modules.contains_key(&module) {
            module = module.rsplit_once('.').map(|(head, _)| head.to_string()).unwrap_or_default();
        }
        if module.starts_with("eve_bridge") {
            let parts: Vec<&str> = module.split('.').collect();
            for i in 1..=parts.len() {
                let prefix = parts[..i].join(".");
                if modules.contains_key(&prefix) {
                    found.insert(prefix);
                }
            }
        }
    }
    found
}

fn closure(roots: &[String], modules: &BTreeMap<String, PathBuf>, top_only: bool) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut todo: Vec<String> = roots.iter().filter(|r| modules.contains_key(*r)).cloned().collect();
    while let Some(module) = todo.pop() {
        if seen.insert(module.clone()) {
            todo.extend(imports_of(&module, &modules[&module], modules, top_only).into_iter().filter(|m| !seen.contains(m)));
        }
    }
    seen
}

fn lite_module_set(modules: &BTreeMap<String, PathBuf>) -> BTreeSet<String> {
    let roots: Vec<String> = COMPOSITION.iter().chain(LITE_MODULES).map(|m| format!("eve_bridge.{m}")).collect();
    let mut set = closure(&roots, modules, false);
    set.extend(closure(&["eve_bridge.cli".to_string()], modules, true));
    set
}

/// Each EVE_* name these modules read that LITE_CLASSIFIED has no reason for, with the modules that read it.
fn unclassified(sources: &BTreeMap<String, String>) -> BTreeMap<String, Vec<String>> {
    let mut missing: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (module, src) in sources {
        for name in switches_in(src) {
            if !profile::LITE_CLASSIFIED.contains_key(&name) {
                missing.entry(name).or_default().push(module.clone());
            }
        }
    }
    missing
}

/// The modules of the set as source, profile.py left out: its names are the registry itself, not reads.
fn lite_sources() -> BTreeMap<String, String> {
    let modules = all_modules();
    lite_module_set(&modules)
        .into_iter()
        .filter(|m| m != "eve_bridge.profile")
        .map(|m| {
            let src = source(&modules[&m]);
            (m, src)
        })
        .collect()
}

// ---- the switches ---------------------------------------------------------------------------------------------------

#[test]
fn test_every_switch_a_lite_module_reads_is_classified() {
    let missing = unclassified(&lite_sources());
    assert!(
        missing.is_empty(),
        "give each name a reason in profile::LITE_CLASSIFIED, or force it in LITE_FORCED: {}",
        missing.iter().map(|(n, m)| format!("{n} ({})", m.join(", "))).collect::<Vec<_>>().join("; ")
    );
}

#[test]
fn test_the_module_set_comes_from_the_imports() {
    // Controls: the composition, what it imports at module level, and what a function imports later are in; the
    // code that runs in other processes (Claude Code's hooks, the Work tab's MCP server, the installer) is out.
    let modules = all_modules();
    let found = lite_module_set(&modules);
    for m in [
        "eve_bridge.telegram", "eve_bridge.records", "eve_bridge.profile", "eve_bridge.toolchain",
        "eve_bridge.websearch", "eve_bridge.app_registry", "eve_bridge.cli",
    ] {
        assert!(found.contains(m), "{m}");
    }
    for m in ["eve_bridge.desk_lane", "eve_bridge.mem0_memory", "eve_bridge.web_reader"] {
        assert!(found.contains(m), "{m} (imported lazily)");
    }
    for m in ["eve_bridge.hooks.pretooluse", "eve_bridge.eve_mcp", "eve_bridge.installer"] {
        assert!(!found.contains(m), "{m}");
    }
    let telegram_top = imports_of("eve_bridge.telegram", &modules["eve_bridge.telegram"], &modules, true);
    assert!(!telegram_top.contains("eve_bridge.desk_lane") && telegram_top.contains("eve_bridge.websearch"));
    for lite_module in LITE_MODULES {
        let name = format!("eve_bridge.{lite_module}");
        if modules.contains_key(&name) {
            assert!(found.contains(&name), "{name}");
        }
    }
}

#[test]
fn test_the_scanner_catches_an_unclassified_switch() {
    // Positive control: a module reading a switch nobody classified, plain or built at run time, is reported.
    let fake = "import os\n\
A = os.environ.get(\"EVE_CANARY_NOT_CLASSIFIED\")\n\
B = os.environ.get(f\"EVE_{name.upper()}_CANARY\")\n\
C = os.environ.get(\"EVE_WATCH\")\n\
D = \"EVE_WATCH is on unless 0\"\n";
    let sources = BTreeMap::from([("eve_bridge.fake".to_string(), fake.to_string())]);
    let want = BTreeMap::from([
        ("EVE_CANARY_NOT_CLASSIFIED".to_string(), vec!["eve_bridge.fake".to_string()]),
        ("EVE_*_CANARY".to_string(), vec!["eve_bridge.fake".to_string()]),
    ]);
    assert_eq!(unclassified(&sources), want);
    let modules = all_modules();
    assert!(switches_in(&source(&modules["eve_bridge.telegram"])).contains("EVE_TELEGRAM"));
    assert!(switches_in(&source(&modules["eve_bridge.desk_lane"])).contains("EVE_DESK_LANE"));
}

#[test]
fn test_the_tokenizer_reads_python_strings_as_ast_does() {
    let src = "x = ('EVE_' 'JOINED')\ny = r'EVE_RAW'\nz = b'EVE_BYTES'\n\
w = f\"{a}EVE_INNER{b!r}\" f'{c[\"EVE_IN_EXPR\"]}'\nv = \"\"\"EVE_TRIPLE\"\"\"\n# 'EVE_COMMENT'\n\
u = f'{{EVE_BRACES}}'\n";
    let found = switches_in(src);
    let want: BTreeSet<String> =
        ["EVE_JOINED", "EVE_RAW", "EVE_INNER", "EVE_IN_EXPR", "EVE_TRIPLE"].iter().map(|s| s.to_string()).collect();
    assert_eq!(found, want);
    assert!(names_in("q = 'PATH'\nr = 'lower'\n").contains("PATH"));
}

#[test]
fn test_each_reason_says_what_it_is_and_holds() {
    let forced = &*profile::LITE_FORCED;
    let defaults = &*profile::LITE_DEFAULTS;
    let kinds = [
        "forced to ", "default when unset: ", "secret-shaped: ", "inert while ", "kept: ", "not run in lite: ",
        "the edition switch",
    ];
    for (name, reason) in profile::LITE_CLASSIFIED.iter() {
        assert!(SWITCH.is_match(name), "{name}");
        assert!(kinds.iter().any(|k| reason.starts_with(k)), "{name}: {reason}");
        assert_eq!(forced.contains_key(name), reason.starts_with("forced to "), "{name}");
        if let Some(value) = forced.get(name) {
            assert_eq!(reason, &format!("forced to {value}"), "{name}");
        }
        assert_eq!(defaults.contains_key(name), reason.starts_with("default when unset: "), "{name}");
        let secret = profile::is_secret_name(name) && !forced.contains_key(name) && !defaults.contains_key(name);
        assert_eq!(secret, reason.starts_with("secret-shaped: "), "{name}");
        if reason.starts_with("inert while ") {
            let head = reason.split(':').next().unwrap();
            let named: Vec<&str> = REASON_SWITCH.find_iter(head).map(|m| m.as_str()).collect();
            assert!(!named.is_empty() && named.iter().all(|n| forced.contains_key(*n)), "{name}: {named:?}");
        }
    }
    assert!(profile::LITE_CLASSIFIED[profile::PROFILE_ENV].starts_with("the edition switch"));
    assert!(forced.keys().all(|k| profile::LITE_CLASSIFIED.contains_key(k)));
    assert!(defaults.keys().all(|k| profile::LITE_CLASSIFIED.contains_key(k)));
}

#[test]
fn test_the_reason_check_catches_a_reason_that_does_not_hold() {
    // Positive control: an inert reason naming a switch lite does not force fails the rule above.
    let reason = "inert while EVE_WATCH is forced off: something";
    let named: Vec<&str> = REASON_SWITCH.find_iter(reason.split(':').next().unwrap()).map(|m| m.as_str()).collect();
    assert_eq!(named, vec!["EVE_WATCH"]);
    assert!(!named.iter().all(|n| profile::LITE_FORCED.contains_key(*n)));
}

// ---- the secrets ----------------------------------------------------------------------------------------------------

#[test]
fn test_every_secret_a_lite_module_reads_is_removed() {
    let names: BTreeSet<String> = lite_sources().values().flat_map(|src| names_in(src)).collect();
    let secretish: BTreeSet<String> = names.into_iter().filter(|n| SECRETISH.is_match(n)).collect();
    for control in ["OPENROUTER_API_KEY", "COMPOSIO_API_KEY", "TINYFISH_API_KEY", "EVE_TELEGRAM_TOKEN"] {
        assert!(secretish.contains(control), "control: {control}");
    }
    let not_secrets = &*profile::NOT_SECRET_NAMES;
    let undecided: Vec<&String> = secretish
        .iter()
        .filter(|n| {
            !profile::is_secret_name(n) && !not_secrets.contains_key(n.as_str()) && !profile::LITE_FORCED.contains_key(*n)
        })
        .collect();
    assert!(
        undecided.is_empty(),
        "a secret goes in profile::SECRET_NAMES (apply removes it); a name that holds none goes in \
profile::NOT_SECRET_NAMES with what it holds: {undecided:?}"
    );
    let mut env: BTreeMap<String, String> =
        BTreeMap::from([("EVE_PROFILE".into(), "lite".into()), ("HOME".into(), "/Users/someone".into())]);
    for n in &secretish {
        env.insert(n.clone(), CANARY.into());
    }
    profile::apply(&mut env).unwrap();
    let left: BTreeSet<String> = env.iter().filter(|(_, v)| *v == CANARY).map(|(k, _)| k.clone()).collect();
    let mut want: BTreeSet<String> = profile::LITE_SECRET_ALLOW.iter().map(|s| s.to_string()).collect();
    want.extend(not_secrets.keys().filter(|k| secretish.contains(**k)).map(|k| k.to_string()));
    assert_eq!(left, want);
}

#[test]
fn test_the_secret_check_catches_a_secret_of_another_shape() {
    // Positive control: a name like FOO_APIKEY mentions a key, is not secret-shaped and is listed nowhere.
    let name = "FOO_APIKEY";
    assert!(SECRETISH.is_match(name) && !profile::is_secret_name(name) && !profile::NOT_SECRET_NAMES.contains_key(name));
}

/// Not a test of the Python one: the scan's module set, switches and names, written where `EVE_PROFILE_SCAN_DUMP`
/// says, to compare with what Python's own ast scan finds.
#[test]
fn dump_scan_when_asked() {
    let Ok(out) = std::env::var("EVE_PROFILE_SCAN_DUMP") else { return };
    let modules = all_modules();
    let set = lite_module_set(&modules);
    let sources = lite_sources();
    let switches: BTreeSet<String> = sources.values().flat_map(|s| switches_in(s)).collect();
    let names: BTreeSet<String> = sources.values().flat_map(|s| names_in(s)).collect();
    let text = format!(
        "{{\"modules\": {:?}, \"switches\": {:?}, \"names\": {:?}}}",
        set.iter().collect::<Vec<_>>(),
        switches.iter().collect::<Vec<_>>(),
        names.iter().collect::<Vec<_>>()
    );
    std::fs::write(out, text).unwrap();
}

