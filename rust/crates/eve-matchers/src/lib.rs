//! Classify Bash commands into allow / ask / default tiers.
//!
//! Port of `bridge/src/eve_bridge/matchers.py`.
//!
//! Rules live in two places:
//!
//! 1. Baked-in defaults at module scope — sensible starting point so a fresh
//!    install works without touching any config file.
//! 2. Optional override at `$XDG_CONFIG_HOME/eve-bridge/matchers.toml`
//!    (defaults to `~/.config/eve-bridge/matchers.toml`). The file can
//!    override or extend either list.
//!
//! TOML shape:
//!
//! ```toml
//! # Commands matching an auto_allow pattern are approved without bothering
//! # the stick. Use for frequent, non-destructive reads.
//! auto_allow = [
//!     "^ls( |$)",
//!     "^cat( |$)",
//! ]
//!
//! # Commands matching an always_ask pattern always go through the stick's
//! # button round-trip, even if the user's default permission mode is "auto".
//! always_ask = [
//!     "^rm( |$)",
//!     "^sudo( |$)",
//! ]
//!
//! # If true, the built-in defaults are discarded and your lists are the only
//! # rules. Default false (your lists extend the built-ins).
//! replace_defaults = false
//!
//! # If true, the stick is the *sole* approval surface: any Bash command not
//! # matching auto_allow routes to the stick for a button press, even ones
//! # the matcher has never seen. Pair this with Claude Code's
//! # `defaultMode: bypassPermissions` to put physical 2FA in front of every
//! # un-vetted command without any terminal prompts in between. Default false
//! # (matcher's "default" class falls through to Claude Code's native flow).
//! strict = false
//! ```
//!
//! Classification precedence: **always_ask beats auto_allow** when both match
//! (the author of a config who explicitly asks for confirmation on `rm`
//! shouldn't be silently overridden by a loose `auto_allow` pattern like
//! `^r.+`).
//!
//! The patterns are Python `re` patterns (the same file feeds the Python
//! daemon); `pyre` rewrites them so they match exactly what Python matches.

mod pycase;
mod pyre;

use std::fmt;
use std::path::{Path, PathBuf};

/// `Decision = Literal["allow", "ask", "default"]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Decision {
    Allow,
    Ask,
    Default,
}

impl Decision {
    /// The string the Python code returns.
    pub fn as_str(self) -> &'static str {
        match self {
            Decision::Allow => "allow",
            Decision::Ask => "ask",
            Decision::Default => "default",
        }
    }
}

impl fmt::Display for Decision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl PartialEq<&str> for Decision {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

// --- baked-in defaults ---------------------------------------------------

/// Read-only / universally safe commands. Users don't need a physical button
/// press to run `ls` every few seconds.
pub const DEFAULT_AUTO_ALLOW: &[&str] = &[
    r"^ls( |$)",
    r"^ll( |$)",
    r"^cat( |$)",
    r"^head( |$)",
    r"^tail( |$)",
    r"^less( |$)",
    r"^more( |$)",
    r"^echo( |$)",
    r"^printf( |$)",
    r"^pwd$",
    r"^whoami$",
    r"^id$",
    r"^uname( |$)",
    r"^date( |$)",
    r"^which( |$)",
    r"^type( |$)",
    r"^file( |$)",
    r"^stat( |$)",
    r"^du( |$)",
    r"^df( |$)",
    r"^wc( |$)",
    r"^grep( |$)",
    r"^rg( |$)",
    r"^ag( |$)",
    r"^fd( |$)",
    r"^find( |$)", // we ask separately for `find ... -delete`; see always_ask
    r"^tree( |$)",
    r"^env( |$)",
    r"^ps( |$)",
    r"^top( |$)",
    r"^free( |$)",
    r"^history( |$)",
    r"^git status( |$)",
    r"^git diff( |$)",
    r"^git log( |$)",
    r"^git show( |$)",
    r"^git branch( |$)",
    r"^git blame( |$)",
    r"^git stash list( |$)",
    r"^git remote -v",
    r"^git config --get ",
    r"^node --version",
    r"^npm --version",
    r"^python --version",
    r"^python3 --version",
    r"^pytest(\s+-[vq])?\s*$", // bare `pytest -q`
];

/// Destructive, networked, privileged, or history-rewriting commands. Always
/// push to the stick even if the user's default permission mode is "auto".
pub const DEFAULT_ALWAYS_ASK: &[&str] = &[
    r"^sudo( |$)",
    r"^su( |$)",
    r"^rm( |$)",
    r"^rmdir( |$)",
    r"^dd( |$)",
    r"^shred( |$)",
    r"^mv ", // mv can clobber — surface it
    r"^chmod( |$)",
    r"^chown( |$)",
    r"^curl( |$)",
    r"^wget( |$)",
    r"^nc ",
    r"^ssh( |$)",
    r"^scp( |$)",
    r"^rsync( |$)",
    r"^ftp( |$)",
    r"^git push( |$)",
    r"^git reset --hard",
    r"^git clean( |$)",
    r"^git rebase( |$)",
    r"^git filter-(branch|repo)",
    r"^git checkout -- ",
    r"^git restore( |$)",
    // `git branch` lists by default, so it sits in auto_allow; any flag that
    // deletes, force-resets, renames, copies or rewires a branch asks — in a
    // short cluster (`-vd`) or long form (`--delete`), anywhere in the line.
    r"^git branch\b.*\s(-[a-zA-Z]*[dDfmMcCu]|--(delete|force|move|copy|set-upstream|unset-upstream|edit-description))",
    r"^git tag -d( |$)",
    r"^docker( |$)",
    r"^podman( |$)",
    r"^kubectl( |$)",
    r"^helm( |$)",
    r"^terraform( |$)",
    r"^aws( |$)",
    r"^gcloud( |$)",
    r"^npm (install|publish|uninstall)( |$)",
    r"^yarn (add|remove|publish)( |$)",
    r"^pnpm (install|add|remove|publish)( |$)",
    r"^pip install",
    r"^pip3 install",
    r"^pipx install",
    r"^brew (install|uninstall|upgrade)( |$)",
    r"^apt(-get)? (install|remove|purge|upgrade)( |$)",
    r"^systemctl( |$)",
    r"^launchctl( |$)",
    r"^kill( |$)",
    r"^killall( |$)",
    r"^pkill( |$)",
    r"^mkfs\b",
    r"^mount( |$)",
    r"^umount( |$)",
    r"^find\b.*-delete\b",
    r"^find\b.*-exec\b",
    r"^xargs\b.*\b(rm|mv|chmod|chown)\b",
    r"^>\s*/dev/", // redirection to /dev/*
    r"^gh (pr|issue|repo) (create|merge|delete|edit)",
];

/// A compiled matcher pattern: Python `re.compile(p)`.
///
/// Holds the Python source (what the owner wrote) and the fancy-regex
/// translation that matches what Python's `re.search` matches.
#[derive(Clone)]
pub struct Pattern {
    source: String,
    regex: fancy_regex::Regex,
    /// The same pattern for the empty string, present when it uses `\B`
    /// (Python's `\B` never matches an empty string).
    empty: Option<fancy_regex::Regex>,
}

/// Why a pattern does not compile (or a search gave up).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PatternError {
    /// `re.error`: a bad regex, which `load_config` skips with a warning.
    #[error("{0}")]
    Bad(String),
    /// Python's `OverflowError` for a repeat count of 4294967295 or more,
    /// which `load_config` does not catch.
    #[error("the repetition number is too large")]
    RepetitionTooLarge,
    /// The backtracking engine gave up on a search (Python would not).
    #[error("{0}")]
    Search(String),
}

impl Pattern {
    /// `re.compile(source)`.
    pub fn new(source: &str) -> Result<Self, PatternError> {
        let (translated, for_empty) = pyre::translate(source).map_err(|e| match e {
            pyre::ReError::Error(m) => PatternError::Bad(m),
            pyre::ReError::Overflow => PatternError::RepetitionTooLarge,
        })?;
        let build =
            |p: &str| fancy_regex::Regex::new(p).map_err(|e| PatternError::Bad(e.to_string()));
        Ok(Pattern {
            source: source.to_string(),
            regex: build(&translated)?,
            empty: for_empty.as_deref().map(build).transpose()?,
        })
    }

    /// The pattern as written (`Pattern.pattern`).
    pub fn as_str(&self) -> &str {
        &self.source
    }

    /// `pat.search(text) is not None`.
    ///
    /// `Err` only when the backtracking engine gives up on a pathological
    /// pattern (Python would keep going); the caller picks the safe side.
    pub fn search(&self, text: &str) -> Result<bool, PatternError> {
        let regex = match (&self.empty, text.is_empty()) {
            (Some(empty), true) => empty,
            _ => &self.regex,
        };
        regex
            .is_match(text)
            .map_err(|e| PatternError::Search(e.to_string()))
    }
}

impl fmt::Debug for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "re.compile({})", pyre::py_repr(&self.source))
    }
}

/// `MatcherConfig` (a frozen dataclass).
#[derive(Debug, Clone, Default)]
pub struct MatcherConfig {
    pub auto_allow: Vec<Pattern>,
    pub always_ask: Vec<Pattern>,
    /// Strict mode: unmatched commands route to the stick instead of falling
    /// through to Claude Code's native flow. See module docs.
    pub strict: bool,
}

/// What `load_config` raises past its own `try`: Python's `TypeError` from
/// `list(...)` or `re.compile(...)` on a value of the wrong type in the TOML
/// (`auto_allow = 5`, `always_ask = [1]`), and the `OverflowError` sre_parse
/// raises for `a{4294967295}`. The Python daemon does not catch either, so a
/// startup with such a file fails; the Rust caller sees the same.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("'{0}' object is not iterable")]
    NotIterable(&'static str),
    #[error("first argument must be string or compiled pattern")]
    NotAPattern,
    /// `re.compile` keys its cache on the pattern, so a list or table entry
    /// fails on hashing before the type check.
    #[error("unhashable type: '{0}'")]
    Unhashable(&'static str),
    #[error("the repetition number is too large")]
    RepetitionTooLarge,
}

/// `_config_path()`: `$XDG_CONFIG_HOME/eve-bridge/matchers.toml`, else
/// `~/.config/eve-bridge/matchers.toml`.
pub fn config_path() -> PathBuf {
    config_path_from(std::env::var_os("XDG_CONFIG_HOME"), home_dir())
}

/// `_config_path()` with the environment passed in (tests must not mutate
/// the process environment).
pub fn config_path_from(xdg_config_home: Option<std::ffi::OsString>, home: PathBuf) -> PathBuf {
    match xdg_config_home {
        Some(base) if !base.is_empty() => {
            PathBuf::from(base).join("eve-bridge").join("matchers.toml")
        }
        _ => home
            .join(".config")
            .join("eve-bridge")
            .join("matchers.toml"),
    }
}

/// `Path.home()`: `$HOME` when set, else the password database.
fn home_dir() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(h) => home_from_env(&h),
        None => dirs::home_dir().unwrap_or_default(),
    }
}

/// `os.path.expanduser("~")` with `$HOME` set: trailing slashes are
/// stripped, and a value left empty (`HOME=` or `HOME=/`) is `/`.
fn home_from_env(home: &std::ffi::OsStr) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    let mut bytes = home.as_bytes();
    while let [rest @ .., b'/'] = bytes {
        bytes = rest;
    }
    if bytes.is_empty() {
        PathBuf::from("/")
    } else {
        PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
    }
}

/// `_compile(patterns)`: compiles each, warning about and dropping a bad
/// regex. A non-string entry is Python's uncaught `TypeError`.
fn compile(patterns: &[PyValue]) -> Result<Vec<Pattern>, ConfigError> {
    let mut out = Vec::with_capacity(patterns.len());
    for p in patterns {
        let p = match p {
            PyValue::Str(p) => p,
            PyValue::Other(name @ ("list" | "dict")) => return Err(ConfigError::Unhashable(name)),
            PyValue::Other(_) => return Err(ConfigError::NotAPattern),
        };
        match Pattern::new(p) {
            Ok(pat) => out.push(pat),
            Err(PatternError::RepetitionTooLarge) => return Err(ConfigError::RepetitionTooLarge),
            Err(e) => {
                tracing::warn!("matchers: ignoring bad regex {} ({})", pyre::py_repr(p), e);
            }
        }
    }
    Ok(out)
}

/// The slice of Python semantics `load_config` applies to TOML values: an
/// entry of a pattern list after `list(...)`.
#[derive(Debug, Clone)]
enum PyValue {
    Str(String),
    /// Any other value, by its Python type name.
    Other(&'static str),
}

fn strs(items: &[&str]) -> Vec<PyValue> {
    items
        .iter()
        .map(|s| PyValue::Str((*s).to_string()))
        .collect()
}

/// Python truthiness of a value `tomllib` produced.
fn truthy(v: &toml::Value) -> bool {
    match v {
        toml::Value::String(s) => !s.is_empty(),
        toml::Value::Integer(i) => *i != 0,
        toml::Value::Float(f) => *f != 0.0,
        toml::Value::Boolean(b) => *b,
        // datetime / date / time objects are always true.
        toml::Value::Datetime(_) => true,
        toml::Value::Array(a) => !a.is_empty(),
        toml::Value::Table(t) => !t.is_empty(),
    }
}

/// Python's `type(v).__name__` as `TypeError` messages print it.
fn py_type_name(v: &toml::Value) -> &'static str {
    match v {
        toml::Value::String(_) => "str",
        toml::Value::Integer(_) => "int",
        toml::Value::Float(_) => "float",
        toml::Value::Boolean(_) => "bool",
        toml::Value::Datetime(d) => match (d.date.is_some(), d.time.is_some()) {
            (true, true) => "datetime.datetime",
            (true, false) => "datetime.date",
            _ => "datetime.time",
        },
        toml::Value::Array(_) => "list",
        toml::Value::Table(_) => "dict",
    }
}

/// `list(data.get(key) or [])`: a list's items, a string's characters, a
/// table's keys; any other truthy value is not iterable.
fn py_list(v: Option<&toml::Value>) -> Result<Vec<PyValue>, ConfigError> {
    let Some(v) = v.filter(|v| truthy(v)) else {
        return Ok(Vec::new());
    };
    match v {
        toml::Value::Array(items) => Ok(items
            .iter()
            .map(|item| match item {
                toml::Value::String(s) => PyValue::Str(s.clone()),
                other => PyValue::Other(py_type_name(other)),
            })
            .collect()),
        toml::Value::String(s) => Ok(s.chars().map(|c| PyValue::Str(c.to_string())).collect()),
        toml::Value::Table(t) => Ok(t.keys().map(|k| PyValue::Str(k.clone())).collect()),
        other => Err(ConfigError::NotIterable(py_type_name(other))),
    }
}

fn read_toml(target: &Path) -> Result<toml::Table, String> {
    let bytes = std::fs::read(target).map_err(|e| e.to_string())?;
    let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
    // tomllib decodes plain UTF-8, so a byte-order mark is a stray character
    // at line 1 and the file fails to parse.
    if text.starts_with('\u{feff}') {
        return Err("Invalid statement (at line 1, column 1)".to_string());
    }
    toml_1_0_only(&text)?;
    text.parse::<toml::Table>().map_err(|e| e.to_string())
}

/// Refuses the TOML 1.1 syntax the `toml` crate accepts and Python's
/// `tomllib` (TOML 1.0) does not, so a file that only parses as 1.1 falls
/// back to the defaults in both daemons: a newline or a trailing comma in an
/// inline table, `\e` and `\xHH` escapes in basic strings, and a time
/// without seconds. Anything else is left to the parser.
fn toml_1_0_only(text: &str) -> Result<(), String> {
    let c: Vec<char> = text.chars().collect();
    let mut line = 1;
    let fail = |what: &str, line: usize| {
        Err(format!(
            "{what} is TOML 1.1, which tomllib (TOML 1.0) rejects (at line {line})"
        ))
    };
    // Open `{` / `[` outside strings and comments, innermost last.
    let mut open: Vec<char> = Vec::new();
    // The last character outside strings, comments and whitespace.
    let mut last = '\n';
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        match ch {
            '\n' => {
                if open.last() == Some(&'{') {
                    return fail("a newline inside an inline table", line);
                }
                line += 1;
                i += 1;
            }
            '#' => {
                while i < c.len() && c[i] != '\n' {
                    i += 1;
                }
            }
            '"' | '\'' => {
                let multi = c.get(i + 1) == Some(&ch) && c.get(i + 2) == Some(&ch);
                i += if multi { 3 } else { 1 };
                loop {
                    let Some(&s) = c.get(i) else {
                        // Unterminated: the parser reports it.
                        return Ok(());
                    };
                    if s == '\n' {
                        if !multi {
                            return Ok(());
                        }
                        line += 1;
                    }
                    if s == '\\' && ch == '"' {
                        match c.get(i + 1) {
                            Some('e') => return fail("the escape \\e", line),
                            Some('x') => return fail("the escape \\x", line),
                            Some('\n') => line += 1,
                            _ => {}
                        }
                        i += 2;
                        continue;
                    }
                    if s == ch {
                        if !multi {
                            i += 1;
                            break;
                        }
                        // A closing `"""` may carry one or two quotes of
                        // content before it.
                        let mut n = 0;
                        while c.get(i + n) == Some(&ch) {
                            n += 1;
                        }
                        if n >= 3 {
                            i += n.min(5);
                            break;
                        }
                        i += n;
                        continue;
                    }
                    i += 1;
                }
                last = ch;
            }
            '{' | '[' => {
                open.push(ch);
                last = ch;
                i += 1;
            }
            '}' | ']' => {
                if ch == '}' && open.last() == Some(&'{') && last == ',' {
                    return fail("a trailing comma in an inline table", line);
                }
                open.pop();
                last = ch;
                i += 1;
            }
            '0'..='9' => {
                // `HH:MM` not followed by `:SS` (a time or the time of a
                // datetime). Outside strings a `:` only appears in times and
                // in the `+HH:MM` / `-HH:MM` offset that follows seconds.
                let after_digit_or_colon =
                    i > 0 && matches!(c[i - 1], '0'..='9' | ':' | '+' | '-');
                let hh_mm = c.get(i + 1).is_some_and(char::is_ascii_digit)
                    && c.get(i + 2) == Some(&':')
                    && c.get(i + 3).is_some_and(char::is_ascii_digit)
                    && c.get(i + 4).is_some_and(char::is_ascii_digit);
                if !after_digit_or_colon && hh_mm && c.get(i + 5) != Some(&':') {
                    return fail("a time without seconds", line);
                }
                last = ch;
                i += 1;
            }
            _ => {
                if !ch.is_whitespace() {
                    last = ch;
                }
                i += 1;
            }
        }
    }
    Ok(())
}

/// Load rules. Falls back to built-in defaults if the file is missing or bad.
///
/// `path: None` reads `config_path()`.
pub fn load_config(path: Option<&Path>) -> Result<MatcherConfig, ConfigError> {
    let default_target;
    let target = match path {
        Some(p) => p,
        None => {
            default_target = config_path();
            &default_target
        }
    };

    let mut auto_allow_patterns = strs(DEFAULT_AUTO_ALLOW);
    let mut always_ask_patterns = strs(DEFAULT_ALWAYS_ASK);
    let mut strict = false;

    if target.exists() {
        let data = match read_toml(target) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(
                    "matchers: failed to parse {} ({}); using defaults",
                    target.display(),
                    e
                );
                return Ok(MatcherConfig {
                    auto_allow: compile(&auto_allow_patterns)?,
                    always_ask: compile(&always_ask_patterns)?,
                    strict,
                });
            }
        };

        let replace = data.get("replace_defaults").is_some_and(truthy);
        strict = data.get("strict").is_some_and(truthy);
        let user_auto_allow = py_list(data.get("auto_allow"))?;
        let user_always_ask = py_list(data.get("always_ask"))?;

        if replace {
            auto_allow_patterns = user_auto_allow;
            always_ask_patterns = user_always_ask;
        } else {
            auto_allow_patterns.extend(user_auto_allow);
            always_ask_patterns.extend(user_always_ask);
        }
    }

    Ok(MatcherConfig {
        auto_allow: compile(&auto_allow_patterns)?,
        always_ask: compile(&always_ask_patterns)?,
        strict,
    })
}

// Characters the shell reads as structure: a separator, a pipe, a redirection,
// a substitution, a grouping, a quote or an escape. A command holding one can
// run more than its first word, and quotes can hide a flag from the word check
// below, so it is never auto-allowed.
const SHELL_META: &str = ";&|<>$`(){}\\'\"#!";
const GLOB: &str = "*?[";
// Programs that run another command taken from their arguments. Bare they are
// harmless (`env` prints the environment); given any argument they are not.
const RUNNERS: &[&str] = &[
    "env", "xargs", "nice", "nohup", "timeout", "time", "command", "exec", "eval", "watch", "sh",
    "bash", "zsh", "sudo", "doas", "su",
];

// Read-only programs with a flag that runs a program or writes a file: the
// long-flag prefixes, then the letters that do it inside a short-flag cluster
// (`fd -Hx rm`, `tree -ao out`).
fn run_or_write_flags(prog: &str) -> Option<(&'static [&'static str], &'static str)> {
    match prog {
        "find" => Some((&["-exec", "-ok", "-delete", "-fprint", "-fls"], "")),
        "fd" => Some((&["--exec"], "xX")),
        "rg" => Some((&["--pre"], "")),
        "tree" => Some((&[], "o")),
        "git" => Some((&["--output", "--ext-diff", "-c"], "")),
        _ => None,
    }
}

/// Python's `str.isspace()` for one character.
fn py_isspace(c: char) -> bool {
    matches!(
        c,
        '\t'..='\r'
            | '\u{1c}'..='\u{20}'
            | '\u{85}'
            | '\u{a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
    )
}

/// Whether a command is one simple command that runs only its first word
/// and writes nothing — the only shape the allow tier may answer.
///
/// The allow patterns are anchored at the start, so without this `echo x;
/// rm -rf ~` matched `^echo( |$)`. verification/Eve/Command.lean models this
/// function and proves allow implies it for every matcher config.
pub fn is_simple(command: &str) -> bool {
    if command
        .chars()
        .any(|c| SHELL_META.contains(c) || (py_isspace(c) && c != ' '))
    {
        return false;
    }
    let words: Vec<&str> = command.split(' ').filter(|w| !w.is_empty()).collect();
    let Some((&prog, args)) = words.split_first() else {
        return false;
    };
    if prog.contains('=') {
        // `FOO=x prog` sets the environment of what runs
        return false;
    }
    if RUNNERS.contains(&prog) {
        return args.is_empty();
    }
    let Some((prefixes, letters)) = run_or_write_flags(prog) else {
        return true;
    };
    if command.chars().any(|c| GLOB.contains(c)) {
        // a file named `--pre=sh` would do
        return false;
    }
    for w in args {
        if prefixes.iter().any(|p| w.starts_with(p)) {
            return false;
        }
        if w.starts_with('-') && !w.starts_with("--") && letters.chars().any(|c| w.contains(c)) {
            return false;
        }
    }
    true
}

/// Returns `allow` | `ask` | `default`.
///
/// always_ask beats auto_allow when both match, so a loose allow pattern can't
/// accidentally silence a deliberate ask pattern. Under `cfg.strict`, an
/// otherwise-unmatched command becomes "ask" instead of "default" so the
/// stick is the sole approval surface. An allow pattern answers only a
/// command that `is_simple`; anything else falls through as unmatched.
pub fn classify_command(command: &str, cfg: &MatcherConfig) -> Decision {
    let fallthrough = if cfg.strict {
        Decision::Ask
    } else {
        Decision::Default
    };
    if command.is_empty() {
        return fallthrough;
    }
    for pat in &cfg.always_ask {
        match pat.search(command) {
            Ok(true) => return Decision::Ask,
            Ok(false) => {}
            Err(e) => {
                // Python would finish the search; a pattern that cannot be
                // decided is treated as matching, the side that asks.
                tracing::warn!(
                    "matchers: always_ask {} gave up ({}); asking",
                    pyre::py_repr(pat.as_str()),
                    e
                );
                return Decision::Ask;
            }
        }
    }
    if is_simple(command) {
        for pat in &cfg.auto_allow {
            match pat.search(command) {
                Ok(true) => return Decision::Allow,
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(
                        "matchers: auto_allow {} gave up ({}); not allowing on it",
                        pyre::py_repr(pat.as_str()),
                        e
                    );
                }
            }
        }
    }
    fallthrough
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_follows_expanduser() {
        assert_eq!(home_from_env("".as_ref()), PathBuf::from("/"));
        assert_eq!(home_from_env("//".as_ref()), PathBuf::from("/"));
        assert_eq!(home_from_env("/x/".as_ref()), PathBuf::from("/x"));
        assert_eq!(home_from_env("/Users/me".as_ref()), PathBuf::from("/Users/me"));
    }

    #[test]
    fn toml_1_1_syntax_is_refused() {
        for doc in [
            "a = {x = 1,\n y = 2}",
            "a = {x = 1, y = 2,}",
            "a = [ { b = 1, } ]",
            "a = {x = 1 # c\n}",
            "a = \"\\e\"",
            "\"k\\x41\" = 1",
            "a = \"\"\"\\e\"\"\"",
            "a = 07:32",
            "a = 1979-05-27T07:32Z",
            "a = 1979-05-27 07:32",
        ] {
            assert!(toml_1_0_only(doc).is_err(), "{doc:?}");
        }
        for doc in [
            "a = {x = 1, y = [1,\n2]}",
            "a = [\n  1, # c\n  2,\n]",
            "a = '\\e'",
            "a = '''\\x41'''",
            "a = \"\\\\e\"",
            "a = \"\"\"a\"\"\"\"\nb = \"\\u0041\"",
            "a = 07:32:00",
            "a = 1979-05-27T07:32:00-07:00",
            "a = 1979-05-27T07:32:00.5+01:00",
            "a = \"07:32\" # 07:32",
            "a = { x = \"}\", y = \"#,\" }",
        ] {
            assert!(toml_1_0_only(doc).is_ok(), "{doc:?}");
        }
    }
}
