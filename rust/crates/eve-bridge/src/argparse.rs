//! The part of Python's `argparse` (3.13) that cli.py's two command lines use, so `eve-bridge` parses, refuses and
//! prints help as the Python CLI does: the same usage lines, the same help layout (HelpFormatter's columns and
//! textwrap's line breaks), the same error lines on stderr with exit 2, unique-prefix long options, `--opt=value`,
//! negative numbers as values, subcommands with aliases, `nargs` `?`, `2` and REMAINDER.

use std::io::Write;

use indexmap::IndexMap;

pub const SUPPRESS: &str = "==SUPPRESS==";

/// A parsed value: Python's object in the namespace.
#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    None,
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    /// `nargs=N` with `action="append"`: one list per use.
    Lists(Vec<Vec<String>>),
    /// `nargs=REMAINDER`.
    List(Vec<String>),
}

impl Val {
    pub fn str(&self) -> Option<&str> {
        match self {
            Val::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn flag(&self) -> bool {
        matches!(self, Val::Bool(true))
    }
    pub fn float(&self) -> f64 {
        match self {
            Val::Float(f) => *f,
            Val::Int(i) => *i as f64,
            _ => 0.0,
        }
    }
    pub fn int(&self) -> Option<i64> {
        match self {
            Val::Int(i) => Some(*i),
            _ => None,
        }
    }
}

/// The namespace `parse_args` returns: every dest, the subcommands' merged in.
#[derive(Debug, Clone, Default)]
pub struct Namespace(pub IndexMap<String, Val>);

impl Namespace {
    pub fn get(&self, dest: &str) -> &Val {
        self.0.get(dest).unwrap_or(&Val::None)
    }
    pub fn str(&self, dest: &str) -> Option<String> {
        self.get(dest).str().map(str::to_string)
    }
    pub fn flag(&self, dest: &str) -> bool {
        self.get(dest).flag()
    }
    pub fn has(&self, dest: &str) -> bool {
        self.0.contains_key(dest)
    }
}

/// A `type=` callable: the value, or the message after `argument X: `.
pub type Conv = fn(&str) -> Result<Val, String>;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Nargs {
    One,
    Optional,
    Exact(usize),
    Remainder,
    /// `nargs="*"`.
    ZeroOrMore,
    /// `nargs="+"`.
    OneOrMore,
    Parser,
}

#[derive(Debug, Clone)]
pub enum Kind {
    Store,
    StoreTrue,
    Append,
    Help,
    Version(String),
    Subparsers(Box<Subparsers>),
}

#[derive(Debug, Clone)]
pub struct Arg {
    pub dest: String,
    pub options: Vec<String>,
    pub kind: Kind,
    pub nargs: Nargs,
    pub default: Val,
    pub choices: Option<Vec<String>>,
    pub help: Option<String>,
    pub metavar: Option<Vec<String>>,
    pub required: bool,
    pub conv: Option<Conv>,
}

impl Arg {
    fn new(dest: &str, options: &[&str]) -> Arg {
        Arg {
            dest: dest.to_string(),
            options: options.iter().map(|s| s.to_string()).collect(),
            kind: Kind::Store,
            nargs: Nargs::One,
            default: Val::None,
            choices: None,
            help: None,
            metavar: None,
            required: false,
            conv: None,
        }
    }

    /// A positional argument.
    pub fn pos(dest: &str) -> Arg {
        let mut a = Arg::new(dest, &[]);
        a.required = true;
        a
    }

    /// An option (`-n`, `--last`); its dest is the first long option's name, as argparse derives it.
    pub fn opt(options: &[&str]) -> Arg {
        let long = options.iter().find(|o| o.starts_with("--")).unwrap_or(&options[0]);
        let dest = long.trim_start_matches('-').replace('-', "_");
        Arg::new(&dest, options)
    }

    pub fn dest(mut self, dest: &str) -> Arg {
        self.dest = dest.to_string();
        self
    }
    pub fn help(mut self, help: &str) -> Arg {
        self.help = Some(help.to_string());
        self
    }
    pub fn default(mut self, v: Val) -> Arg {
        self.default = v;
        self
    }
    pub fn default_str(self, s: &str) -> Arg {
        self.default(Val::Str(s.to_string()))
    }
    pub fn choices(mut self, c: &[&str]) -> Arg {
        self.choices = Some(c.iter().map(|s| s.to_string()).collect());
        self
    }
    pub fn metavar(mut self, m: &[&str]) -> Arg {
        self.metavar = Some(m.iter().map(|s| s.to_string()).collect());
        self
    }
    pub fn optional(mut self) -> Arg {
        self.nargs = Nargs::Optional;
        self.required = false;
        self
    }
    pub fn remainder(mut self) -> Arg {
        self.nargs = Nargs::Remainder;
        self.required = false;
        self
    }
    /// `nargs="*"`.
    pub fn zero_or_more(mut self) -> Arg {
        self.nargs = Nargs::ZeroOrMore;
        self.required = false;
        self
    }
    /// `nargs="+"`.
    pub fn one_or_more(mut self) -> Arg {
        self.nargs = Nargs::OneOrMore;
        self
    }
    pub fn flag(mut self) -> Arg {
        self.kind = Kind::StoreTrue;
        self.default = Val::Bool(false);
        self
    }
    pub fn append(mut self, n: usize) -> Arg {
        self.kind = Kind::Append;
        self.nargs = Nargs::Exact(n);
        self
    }
    pub fn required(mut self) -> Arg {
        self.required = true;
        self
    }
    pub fn conv(mut self, c: Conv) -> Arg {
        self.conv = Some(c);
        self
    }
    pub fn int(self) -> Arg {
        self.conv(conv_int)
    }
    pub fn float(self) -> Arg {
        self.conv(conv_float)
    }

    fn positional(&self) -> bool {
        self.options.is_empty()
    }

    fn suppressed(&self) -> bool {
        self.help.as_deref() == Some(SUPPRESS)
    }

    /// `_get_action_name`.
    fn name(&self) -> String {
        if !self.options.is_empty() {
            return self.options.join("/");
        }
        if let Some(m) = &self.metavar {
            return m.join(" ");
        }
        self.dest.clone()
    }

    fn takes(&self) -> usize {
        match (&self.kind, self.nargs) {
            (Kind::StoreTrue | Kind::Help | Kind::Version(_), _) => 0,
            (_, Nargs::Exact(n)) => n,
            _ => 1,
        }
    }
}

/// One subcommand parser in `add_subparsers`, with its aliases and help (None: added without help).
#[derive(Debug, Clone)]
pub struct Choice {
    pub name: String,
    pub aliases: Vec<String>,
    pub help: Option<String>,
    pub parser: Parser,
}

#[derive(Debug, Clone)]
pub struct Subparsers {
    pub choices: Vec<Choice>,
}

impl Subparsers {
    fn names(&self) -> Vec<String> {
        let mut out = Vec::new();
        for c in &self.choices {
            out.push(c.name.clone());
            out.extend(c.aliases.iter().cloned());
        }
        out
    }

    fn find(&self, word: &str) -> Option<&Choice> {
        self.choices.iter().find(|c| c.name == word || c.aliases.iter().any(|a| a == word))
    }
}

#[derive(Debug, Clone)]
pub struct Parser {
    pub prog: String,
    pub description: Option<String>,
    pub args: Vec<Arg>,
}

/// What stops a parse: help or version printed (0), or an error printed (2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exit(pub i32);

impl Parser {
    /// `ArgumentParser(prog=..., description=..., add_help=...)`.
    pub fn new(prog: &str, description: Option<&str>, add_help: bool) -> Parser {
        let mut p = Parser { prog: prog.to_string(), description: description.map(str::to_string), args: Vec::new() };
        if add_help {
            let mut h = Arg::opt(&["-h", "--help"]).help("show this help message and exit");
            h.kind = Kind::Help;
            h.dest = SUPPRESS.into();
            p.args.push(h);
        }
        p
    }

    pub fn arg(&mut self, a: Arg) -> &mut Parser {
        self.args.push(a);
        self
    }

    /// `add_argument("--version", action="version", version=...)`.
    pub fn version(&mut self, text: &str) -> &mut Parser {
        let mut v = Arg::opt(&["--version"]).help("show program's version number and exit");
        v.kind = Kind::Version(text.to_string());
        v.dest = SUPPRESS.into();
        self.args.push(v);
        self
    }

    /// `add_subparsers(dest=...)` with its parsers; each child's prog is `<prog> <name>`.
    pub fn subparsers(&mut self, dest: &str, choices: Vec<(&str, &[&str], Option<&str>, Parser)>) -> &mut Parser {
        let mut a = Arg::pos(dest);
        a.required = false;
        a.nargs = Nargs::Parser;
        a.kind = Kind::Subparsers(Box::new(Subparsers {
            choices: choices
                .into_iter()
                .map(|(name, aliases, help, parser)| Choice {
                    name: name.to_string(),
                    aliases: aliases.iter().map(|s| s.to_string()).collect(),
                    help: help.map(str::to_string),
                    parser,
                })
                .collect(),
        }));
        self.args.push(a);
        self
    }

    /// The child parser's prog: `ArgumentParser(prog=...)` as add_parser derives it.
    pub fn child_prog(&self, name: &str) -> String {
        format!("{} {name}", self.prog)
    }

    /// The subcommand parser that `path` names (for printing a parent's help), or None.
    pub fn find(&self, path: &[&str]) -> Option<&Parser> {
        let Some((first, rest)) = path.split_first() else { return Some(self) };
        for a in &self.args {
            if let Kind::Subparsers(s) = &a.kind {
                return s.find(first)?.parser.find(rest);
            }
        }
        None
    }

    /// The subcommand names this parser takes, aliases included (argparse's `choices`).
    pub fn command_names(&self) -> Vec<String> {
        for a in &self.args {
            if let Kind::Subparsers(s) = &a.kind {
                return s.names();
            }
        }
        Vec::new()
    }

    // ---- parsing ----------------------------------------------------------------------------------------------

    /// `parse_args(argv)`: the namespace, or the exit status after argparse's output.
    pub fn parse_args(&self, argv: &[String]) -> Result<Namespace, Exit> {
        self.parse_args_to(argv, &mut std::io::stdout(), &mut std::io::stderr())
    }

    pub fn parse_args_to(&self, argv: &[String], out: &mut dyn Write, err: &mut dyn Write) -> Result<Namespace, Exit> {
        let mut ns = Namespace::default();
        let mut extras = Vec::new();
        self.parse_known(argv, &mut ns, &mut extras, out, err)?;
        if !extras.is_empty() {
            return Err(self.error(&format!("unrecognized arguments: {}", extras.join(" ")), err));
        }
        Ok(ns)
    }

    fn error(&self, message: &str, err: &mut dyn Write) -> Exit {
        let _ = write!(err, "{}", self.format_usage());
        let _ = writeln!(err, "{}: error: {message}", self.prog);
        let _ = err.flush();
        Exit(2)
    }

    fn option_index(&self) -> Vec<(&str, usize)> {
        let mut out = Vec::new();
        for (i, a) in self.args.iter().enumerate() {
            for o in &a.options {
                out.push((o.as_str(), i));
            }
        }
        out
    }

    /// `_negative_number_matcher` and whether any option looks like one (none here do).
    fn looks_negative(s: &str) -> bool {
        let b = s.strip_prefix('-').unwrap_or("");
        if b.is_empty() {
            return false;
        }
        // ^-\d+$|^-\d*\.\d+$
        if b.chars().all(|c| c.is_ascii_digit()) {
            return true;
        }
        if let Some((l, r)) = b.split_once('.') {
            return l.chars().all(|c| c.is_ascii_digit()) && !r.is_empty() && r.chars().all(|c| c.is_ascii_digit());
        }
        false
    }

    /// `_parse_optional`: None for a positional string; Some((action index or None for an unknown option,
    /// the option string, an explicit `=value`)). Err for an ambiguous abbreviation.
    #[allow(clippy::type_complexity)]
    fn classify(&self, arg: &str) -> Result<Option<(Option<usize>, String, Option<String>)>, String> {
        if !arg.starts_with('-') {
            return Ok(None);
        }
        let index = self.option_index();
        if let Some((_, i)) = index.iter().find(|(o, _)| *o == arg) {
            return Ok(Some((Some(*i), arg.to_string(), None)));
        }
        if arg.len() == 1 {
            return Ok(None);
        }
        if let Some((opt, val)) = arg.split_once('=') {
            if let Some((_, i)) = index.iter().find(|(o, _)| *o == opt) {
                return Ok(Some((Some(*i), opt.to_string(), Some(val.to_string()))));
            }
        }
        // abbreviations (allow_abbrev)
        let mut matches: Vec<(usize, String, Option<String>)> = Vec::new();
        if arg.starts_with("--") {
            let (prefix, explicit) = match arg.split_once('=') {
                Some((p, v)) => (p, Some(v.to_string())),
                None => (arg, None),
            };
            for (o, i) in &index {
                if o.starts_with(prefix) {
                    matches.push((*i, o.to_string(), explicit.clone()));
                }
            }
        } else {
            // a single-dash option with its value attached (`-n5`), or flags run together
            let prefix: String = arg.chars().take(2).collect();
            let explicit: String = arg.chars().skip(2).collect();
            for (o, i) in &index {
                if *o == prefix {
                    matches.push((*i, o.to_string(), Some(explicit.clone())));
                } else if o.starts_with(arg) {
                    matches.push((*i, o.to_string(), None));
                }
            }
        }
        if matches.len() > 1 {
            let names: Vec<&str> = matches.iter().map(|m| m.1.as_str()).collect();
            return Err(format!("ambiguous option: {} could match {}", arg.split('=').next().unwrap_or(arg), names.join(", ")));
        }
        if let Some((i, o, explicit)) = matches.pop() {
            return Ok(Some((Some(i), o, explicit)));
        }
        if Self::looks_negative(arg) {
            return Ok(None);
        }
        if arg.contains(' ') {
            return Ok(None);
        }
        Ok(Some((None, arg.to_string(), None)))
    }

    /// Single-dash flags run together (`-yf`) as argparse reads them: one flag at a time while each is one.
    fn split_flags(&self, s: &str) -> Vec<String> {
        let chars: Vec<char> = s.chars().collect();
        if chars.len() <= 2 || chars[0] != '-' || chars[1] == '-' || self.option_index().iter().any(|(o, _)| *o == s) {
            return vec![s.to_string()];
        }
        let head: String = chars[..2].iter().collect();
        let flag = self
            .option_index()
            .iter()
            .find(|(o, _)| *o == head)
            .is_some_and(|(_, i)| self.args[*i].takes() == 0);
        if !flag {
            return vec![s.to_string()];
        }
        let mut out = vec![head];
        out.extend(self.split_flags(&format!("-{}", chars[2..].iter().collect::<String>())));
        out
    }

    fn convert(&self, a: &Arg, s: &str, err: &mut dyn Write) -> Result<Val, Exit> {
        let v = match a.conv {
            Some(c) => c(s).map_err(|m| self.error(&format!("argument {}: {m}", a.name()), err))?,
            None => Val::Str(s.to_string()),
        };
        if let (Some(choices), Val::Str(text)) = (&a.choices, &v) {
            if !choices.contains(text) {
                let listed: Vec<String> = choices.iter().map(|c| py_repr(c)).collect();
                return Err(self.error(
                    &format!("argument {}: invalid choice: {} (choose from {})", a.name(), py_repr(text), listed.join(", ")),
                    err,
                ));
            }
        }
        Ok(v)
    }

    fn parse_known(
        &self,
        argv: &[String],
        ns: &mut Namespace,
        extras: &mut Vec<String>,
        out: &mut dyn Write,
        err: &mut dyn Write,
    ) -> Result<(), Exit> {
        for a in &self.args {
            if a.dest != SUPPRESS && !ns.has(&a.dest) {
                ns.0.insert(a.dest.clone(), a.default.clone());
            }
        }
        // 'O' or 'A' for each string; after the first "--" everything is 'A' and the "--" itself is dropped
        let mut strings: Vec<String> = Vec::new();
        let mut kinds: Vec<Option<(Option<usize>, String, Option<String>)>> = Vec::new();
        let mut dashed = false;
        for s in argv {
            if dashed {
                strings.push(s.clone());
                kinds.push(None);
                continue;
            }
            if s == "--" {
                dashed = true;
                continue;
            }
            for piece in self.split_flags(s) {
                let k = self.classify(&piece).map_err(|m| self.error(&m, err))?;
                strings.push(piece);
                kinds.push(k);
            }
        }
        let positionals: Vec<usize> = (0..self.args.len()).filter(|&i| self.args[i].positional()).collect();
        let mut next_pos = 0usize;
        let mut seen: Vec<bool> = vec![false; self.args.len()];
        let n = strings.len();
        let mut i = 0usize;
        while i < n {
            if let Some((action, _opt, explicit)) = kinds[i].clone() {
                let Some(ai) = action else {
                    extras.push(strings[i].clone());
                    i += 1;
                    continue;
                };
                i += 1;
                let a = &self.args[ai];
                match &a.kind {
                    Kind::Help => {
                        let _ = write!(out, "{}", self.format_help());
                        let _ = out.flush();
                        return Err(Exit(0));
                    }
                    Kind::Version(text) => {
                        let _ = writeln!(out, "{text}");
                        let _ = out.flush();
                        return Err(Exit(0));
                    }
                    Kind::StoreTrue => {
                        if let Some(ex) = explicit {
                            return Err(self.error(
                                &format!("argument {}: ignored explicit argument {}", a.name(), py_repr(&ex)),
                                err,
                            ));
                        }
                        seen[ai] = true;
                        ns.0.insert(a.dest.clone(), Val::Bool(true));
                    }
                    Kind::Store | Kind::Append => {
                        let want = a.takes();
                        let mut values: Vec<String> = Vec::new();
                        if let Some(ex) = explicit {
                            if want != 1 {
                                return Err(self.error(&format!("argument {}: expected {want} arguments", a.name()), err));
                            }
                            values.push(ex);
                        } else {
                            while values.len() < want && i < n && kinds[i].is_none() {
                                values.push(strings[i].clone());
                                i += 1;
                            }
                            if values.len() < want {
                                let m = if want == 1 { "expected one argument".to_string() } else { format!("expected {want} arguments") };
                                return Err(self.error(&format!("argument {}: {m}", a.name()), err));
                            }
                        }
                        seen[ai] = true;
                        if matches!(a.kind, Kind::Append) {
                            let mut converted = Vec::new();
                            for v in &values {
                                match self.convert(a, v, err)? {
                                    Val::Str(s) => converted.push(s),
                                    other => converted.push(format!("{other:?}")),
                                }
                            }
                            let mut lists = match ns.0.get(&a.dest) {
                                Some(Val::Lists(l)) => l.clone(),
                                _ => Vec::new(),
                            };
                            lists.push(converted);
                            ns.0.insert(a.dest.clone(), Val::Lists(lists));
                        } else {
                            let v = self.convert(a, &values[0], err)?;
                            ns.0.insert(a.dest.clone(), v);
                        }
                    }
                    Kind::Subparsers(_) => unreachable!("subparsers are positional"),
                }
                continue;
            }
            // a run of positional strings
            let mut j = i;
            while j < n && kinds[j].is_none() {
                j += 1;
            }
            while i < j && next_pos < positionals.len() {
                let ai = positionals[next_pos];
                let a = &self.args[ai];
                match a.nargs {
                    Nargs::One | Nargs::Optional => {
                        let v = self.convert(a, &strings[i], err)?;
                        ns.0.insert(a.dest.clone(), v);
                        seen[ai] = true;
                        i += 1;
                        next_pos += 1;
                    }
                    Nargs::Exact(k) => {
                        if j - i < k {
                            break;
                        }
                        let vals: Vec<String> = strings[i..i + k].to_vec();
                        ns.0.insert(a.dest.clone(), Val::List(vals));
                        seen[ai] = true;
                        i += k;
                        next_pos += 1;
                    }
                    Nargs::ZeroOrMore | Nargs::OneOrMore => {
                        ns.0.insert(a.dest.clone(), Val::List(strings[i..j].to_vec()));
                        seen[ai] = true;
                        i = j;
                        next_pos += 1;
                    }
                    Nargs::Remainder => {
                        ns.0.insert(a.dest.clone(), Val::List(strings[i..].to_vec()));
                        seen[ai] = true;
                        next_pos += 1;
                        i = n;
                        j = n;
                    }
                    Nargs::Parser => {
                        let Kind::Subparsers(sp) = &a.kind else { unreachable!("a parser positional") };
                        let word = &strings[i];
                        let Some(choice) = sp.find(word) else {
                            let listed: Vec<String> = sp.names().iter().map(|c| py_repr(c)).collect();
                            return Err(self.error(
                                &format!(
                                    "argument {}: invalid choice: {} (choose from {})",
                                    a.name(),
                                    py_repr(word),
                                    listed.join(", ")
                                ),
                                err,
                            ));
                        };
                        ns.0.insert(a.dest.clone(), Val::Str(word.clone()));
                        seen[ai] = true;
                        // everything after the word goes to the subcommand
                        let rest: Vec<String> = strings[i + 1..].to_vec();
                        let mut sub = Namespace::default();
                        choice.parser.parse_known(&rest, &mut sub, extras, out, err)?;
                        for (key, v) in sub.0 {
                            ns.0.insert(key, v);
                        }
                        next_pos += 1;
                        i = n;
                        j = n;
                    }
                }
                // a '?' positional waits for a later run unless this is the end of the arguments
                if i == j && j < n {
                    break;
                }
            }
            while i < j {
                extras.push(strings[i].clone());
                i += 1;
            }
        }
        // positionals left: their defaults (string defaults are converted and checked, as argparse does)
        let mut missing: Vec<String> = Vec::new();
        for &ai in &positionals[next_pos.min(positionals.len())..] {
            let a = &self.args[ai];
            match a.nargs {
                Nargs::Optional => {
                    if let Val::Str(s) = &a.default {
                        let v = self.convert(a, s, err)?;
                        ns.0.insert(a.dest.clone(), v);
                    }
                }
                Nargs::Remainder => {
                    ns.0.insert(a.dest.clone(), Val::List(Vec::new()));
                }
                Nargs::ZeroOrMore => {
                    let v = if a.default == Val::None { Val::List(Vec::new()) } else { a.default.clone() };
                    ns.0.insert(a.dest.clone(), v);
                }
                _ => {
                    if a.required {
                        missing.push(a.name());
                    }
                }
            }
        }
        for (ai, a) in self.args.iter().enumerate() {
            if !a.positional() && a.required && !seen[ai] {
                missing.push(a.name());
            }
        }
        if !missing.is_empty() {
            // argparse lists them in the order the actions were added
            let order: Vec<String> = self.args.iter().filter(|a| a.required).map(Arg::name).collect();
            missing.sort_by_key(|m| order.iter().position(|o| o == m).unwrap_or(usize::MAX));
            return Err(self.error(&format!("the following arguments are required: {}", missing.join(", ")), err));
        }
        Ok(())
    }

    // ---- help -------------------------------------------------------------------------------------------------

    fn metavar_of(a: &Arg) -> Vec<String> {
        if let Some(m) = &a.metavar {
            return m.clone();
        }
        if let Kind::Subparsers(s) = &a.kind {
            return vec![format!("{{{}}}", s.names().join(","))];
        }
        if let Some(c) = &a.choices {
            return vec![format!("{{{}}}", c.join(","))];
        }
        if a.positional() {
            vec![a.dest.clone()]
        } else {
            vec![a.dest.to_uppercase()]
        }
    }

    /// `_format_args`.
    fn format_args(a: &Arg) -> String {
        let m = Self::metavar_of(a);
        let one = |k: usize| m.get(k).or(m.first()).cloned().unwrap_or_default();
        match a.nargs {
            Nargs::One => one(0),
            Nargs::Optional => format!("[{}]", one(0)),
            Nargs::Exact(n) => (0..n).map(one).collect::<Vec<_>>().join(" "),
            Nargs::Remainder => "...".into(),
            Nargs::ZeroOrMore => format!("[{} ...]", one(0)),
            Nargs::OneOrMore => format!("{} [{} ...]", one(0), one(0)),
            Nargs::Parser => format!("{} ...", one(0)),
        }
    }

    fn usage_parts(&self) -> (Vec<String>, Vec<String>) {
        let mut opt = Vec::new();
        let mut pos = Vec::new();
        for a in &self.args {
            if a.suppressed() {
                continue;
            }
            if a.positional() {
                pos.push(Self::format_args(a));
            } else {
                let part = if a.takes() == 0 { a.options[0].clone() } else { format!("{} {}", a.options[0], Self::format_args(a)) };
                opt.push(if a.required { part } else { format!("[{part}]") });
            }
        }
        (opt, pos)
    }

    /// `format_usage()`: `usage: ...\n`.
    pub fn format_usage(&self) -> String {
        format!("usage: {}\n", self.usage_body(width()))
    }

    fn usage_body(&self, width: usize) -> String {
        let prefix = "usage: ";
        let (opt, pos) = self.usage_parts();
        let mut all = vec![self.prog.clone()];
        all.extend(opt.iter().cloned());
        all.extend(pos.iter().cloned());
        let usage = all.join(" ");
        let text_width = width;
        if prefix.len() + usage.chars().count() <= text_width {
            return usage;
        }
        let get_lines = |parts: &[String], indent: &str, prefix: Option<&str>| -> Vec<String> {
            let mut lines: Vec<String> = Vec::new();
            let mut line: Vec<String> = Vec::new();
            let indent_length = indent.chars().count();
            let mut line_len = match prefix {
                Some(p) => p.chars().count() as isize - 1,
                None => indent_length as isize - 1,
            };
            for part in parts {
                let pl = part.chars().count() as isize;
                if line_len + 1 + pl > text_width as isize && !line.is_empty() {
                    lines.push(format!("{indent}{}", line.join(" ")));
                    line.clear();
                    line_len = indent_length as isize - 1;
                }
                line.push(part.clone());
                line_len += pl + 1;
            }
            if !line.is_empty() {
                lines.push(format!("{indent}{}", line.join(" ")));
            }
            if prefix.is_some() && !lines.is_empty() {
                lines[0] = lines[0].chars().skip(indent_length).collect();
            }
            lines
        };
        let prog_len = self.prog.chars().count();
        let lines: Vec<String> = if (prefix.len() + prog_len) as f64 <= 0.75 * text_width as f64 {
            let indent = " ".repeat(prefix.len() + prog_len + 1);
            let mut head = vec![self.prog.clone()];
            if !opt.is_empty() {
                head.extend(opt.iter().cloned());
                let mut l = get_lines(&head, &indent, Some(prefix));
                l.extend(get_lines(&pos, &indent, None));
                l
            } else if !pos.is_empty() {
                head.extend(pos.iter().cloned());
                get_lines(&head, &indent, Some(prefix))
            } else {
                vec![self.prog.clone()]
            }
        } else {
            let indent = " ".repeat(prefix.len());
            let mut parts = opt.clone();
            parts.extend(pos.iter().cloned());
            let mut l = get_lines(&parts, &indent, None);
            if l.len() > 1 {
                l = get_lines(&opt, &indent, None);
                l.extend(get_lines(&pos, &indent, None));
            }
            let mut out = vec![self.prog.clone()];
            out.extend(l);
            out
        };
        lines.join("\n")
    }

    fn invocation(a: &Arg) -> String {
        if a.positional() {
            return Self::metavar_of(a).first().cloned().unwrap_or_default();
        }
        if a.takes() == 0 {
            return a.options.join(", ");
        }
        let mut parts: Vec<String> = a.options[..a.options.len() - 1].to_vec();
        parts.push(format!("{} {}", a.options[a.options.len() - 1], Self::format_args(a)));
        parts.join(", ")
    }

    /// `format_help()`.
    pub fn format_help(&self) -> String {
        self.format_help_width(width())
    }

    pub fn format_help_width(&self, width: usize) -> String {
        let max_help_position = 24usize.min(width.saturating_sub(20).max(4));
        // the longest invocation, sub-actions included
        let mut max_len = 0usize;
        for a in &self.args {
            if a.suppressed() {
                continue;
            }
            max_len = max_len.max(Self::invocation(a).chars().count() + 2);
            if let Kind::Subparsers(s) = &a.kind {
                for c in &s.choices {
                    if c.help.is_some() {
                        max_len = max_len.max(choice_metavar(c).chars().count() + 4);
                    }
                }
            }
        }
        let help_position = (max_len + 2).min(max_help_position);
        let help_width = width.saturating_sub(help_position).max(11);
        let mut out = String::new();
        out.push_str(&format!("usage: {}\n\n", self.usage_body(width)));
        if let Some(d) = &self.description {
            for line in wrap(d, width.max(11)) {
                out.push_str(&line);
                out.push('\n');
            }
            out.push('\n');
        }
        let format_item = |out: &mut String, indent: usize, header: &str, help: Option<&str>| {
            let action_width = help_position.saturating_sub(indent + 2);
            let help = help.filter(|h| !h.trim().is_empty());
            let indent_first;
            match help {
                None => {
                    out.push_str(&format!("{}{header}\n", " ".repeat(indent)));
                    return;
                }
                Some(_) if header.chars().count() <= action_width => {
                    let pad = action_width - header.chars().count();
                    out.push_str(&format!("{}{header}{}  ", " ".repeat(indent), " ".repeat(pad)));
                    indent_first = 0;
                }
                Some(_) => {
                    out.push_str(&format!("{}{header}\n", " ".repeat(indent)));
                    indent_first = help_position;
                }
            }
            let text = expand_help(help.unwrap_or_default());
            let lines = wrap(&text, help_width);
            if let Some((first, rest)) = lines.split_first() {
                out.push_str(&format!("{}{first}\n", " ".repeat(indent_first)));
                for l in rest {
                    out.push_str(&format!("{}{l}\n", " ".repeat(help_position)));
                }
            } else {
                out.push('\n');
            }
        };
        let positionals: Vec<&Arg> = self.args.iter().filter(|a| a.positional() && !a.suppressed()).collect();
        let optionals: Vec<&Arg> = self.args.iter().filter(|a| !a.positional() && !a.suppressed()).collect();
        if !positionals.is_empty() {
            out.push_str("positional arguments:\n");
            for a in positionals {
                format_item(&mut out, 2, &Self::invocation(a), a.help.as_deref());
                if let Kind::Subparsers(s) = &a.kind {
                    for c in &s.choices {
                        if let Some(h) = &c.help {
                            format_item(&mut out, 4, &choice_metavar(c), Some(h));
                        }
                    }
                }
            }
            out.push('\n');
        }
        if !optionals.is_empty() {
            out.push_str("options:\n");
            for a in optionals {
                format_item(&mut out, 2, &Self::invocation(a), a.help.as_deref());
            }
            out.push('\n');
        }
        let trimmed = out.trim_end_matches('\n');
        format!("{trimmed}\n")
    }

    /// `print_help()`.
    pub fn print_help(&self) {
        let mut o = std::io::stdout().lock();
        let _ = write!(o, "{}", self.format_help());
        let _ = o.flush();
    }
}

fn choice_metavar(c: &Choice) -> String {
    if c.aliases.is_empty() {
        c.name.clone()
    } else {
        format!("{} ({})", c.name, c.aliases.join(", "))
    }
}

/// `_expand_help`: `%%` is a literal percent (no help here uses `%(...)s`).
fn expand_help(text: &str) -> String {
    text.replace("%%", "%")
}

/// Python's `repr()` of a str.
pub fn py_repr(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(q);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == q => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(q);
    out
}

// ---- the width and textwrap ---------------------------------------------------------------------------------------

/// HelpFormatter's width: `shutil.get_terminal_size().columns - 2` (COLUMNS, else the terminal on stdout, else 80).
pub fn width() -> usize {
    let cols = std::env::var("COLUMNS").ok().and_then(|c| c.trim().parse::<i64>().ok()).filter(|&c| c > 0);
    let cols = cols.or_else(terminal_columns).unwrap_or(80);
    (cols as usize).saturating_sub(2)
}

#[cfg(unix)]
fn terminal_columns() -> Option<i64> {
    // SAFETY: TIOCGWINSZ fills a winsize; a failure (stdout is not a terminal) is reported by the return value.
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_col > 0 {
            return Some(ws.ws_col as i64);
        }
    }
    None
}

#[cfg(not(unix))]
fn terminal_columns() -> Option<i64> {
    None
}

fn is_lt(c: Option<char>) -> bool {
    // [^\d\W]: a word character that is not a digit
    c.is_some_and(|c| (c.is_alphanumeric() || c == '_') && !c.is_numeric())
}

fn is_wp(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_alphanumeric() || c == '_' || "!\"'&.,?".contains(c))
}

fn is_word(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_alphanumeric() || c == '_')
}

/// textwrap's chunks of one whitespace-free word (`wordsep_re` with break_on_hyphens).
fn word_chunks(w: &[char]) -> Vec<String> {
    let at = |k: isize| -> Option<char> { if k < 0 { None } else { w.get(k as usize).copied() } };
    let mut out = Vec::new();
    let mut start = 0usize;
    while start < w.len() {
        // an em-dash between words: (?<=wp) -{2,} (?=\w)
        if w[start] == '-' && at(start as isize + 1) == Some('-') && is_wp(at(start as isize - 1)) {
            let mut e = start;
            while e < w.len() && w[e] == '-' {
                e += 1;
            }
            if is_word(at(e as isize)) {
                out.push(w[start..e].iter().collect());
                start = e;
                continue;
            }
        }
        let mut end = w.len();
        for e in start + 1..=w.len() {
            let ei = e as isize;
            // a hyphenated word: the chunk ends with its hyphen
            if e >= start + 2 && w[e - 1] == '-' {
                let behind = (is_lt(at(ei - 3)) && is_lt(at(ei - 2)))
                    || (is_lt(at(ei - 4)) && at(ei - 3) == Some('-') && is_lt(at(ei - 2)));
                let ahead = is_lt(at(ei)) && (is_lt(at(ei + 1)) || (at(ei + 1) == Some('-') && is_lt(at(ei + 2))));
                if behind && ahead {
                    end = e;
                    break;
                }
            }
            if e == w.len() {
                end = e;
                break;
            }
            // before an em-dash
            if is_wp(at(ei - 1)) && at(ei) == Some('-') && at(ei + 1) == Some('-') {
                let mut k = e;
                while k < w.len() && w[k] == '-' {
                    k += 1;
                }
                if is_word(at(k as isize)) {
                    end = e;
                    break;
                }
            }
        }
        out.push(w[start..end].iter().collect());
        start = end;
    }
    out
}

/// argparse's `_split_lines`: whitespace collapsed, then `textwrap.wrap(text, width)`.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut chunks: Vec<String> = Vec::new();
    for (k, word) in words.iter().enumerate() {
        if k > 0 {
            chunks.push(" ".into());
        }
        let chars: Vec<char> = word.chars().collect();
        chunks.extend(word_chunks(&chars));
    }
    let len = |s: &str| s.chars().count();
    chunks.reverse();
    let mut lines: Vec<String> = Vec::new();
    while !chunks.is_empty() {
        let mut cur: Vec<String> = Vec::new();
        let mut cur_len = 0usize;
        let width = width.max(1);
        if chunks.last().is_some_and(|c| c.trim().is_empty()) && !lines.is_empty() {
            chunks.pop();
        }
        while let Some(c) = chunks.last() {
            let l = len(c);
            if cur_len + l <= width {
                cur_len += l;
                cur.push(chunks.pop().unwrap_or_default());
            } else {
                break;
            }
        }
        if let Some(c) = chunks.last().cloned() {
            if len(&c) > width {
                // _handle_long_word
                let space_left = if width > cur_len { width - cur_len } else { 1 };
                let chars: Vec<char> = c.chars().collect();
                let mut end = space_left;
                if chars.len() > space_left {
                    let head: String = chars[..space_left].iter().collect();
                    if let Some(h) = head.rfind('-') {
                        let hc = head[..h].chars().count();
                        if hc > 0 && chars[..hc].iter().any(|&ch| ch != '-') {
                            end = hc + 1;
                        }
                    }
                }
                let end = end.min(chars.len());
                cur.push(chars[..end].iter().collect());
                let last = chunks.len() - 1;
                chunks[last] = chars[end..].iter().collect();
                if chunks[last].is_empty() {
                    chunks.pop();
                }
            }
        }
        if cur.last().is_some_and(|c| c.trim().is_empty()) {
            cur.pop();
        }
        if !cur.is_empty() {
            lines.push(cur.concat());
        }
    }
    lines
}

// ---- Python's int() and float() as types ---------------------------------------------------------------------------

/// Python's `str.strip()` whitespace.
fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

/// Underscores between digits, as Python's numeric literals allow them.
fn without_underscores(s: &str) -> Option<String> {
    if !s.contains('_') {
        return Some(s.to_string());
    }
    let chars: Vec<char> = s.chars().collect();
    for (k, &c) in chars.iter().enumerate() {
        if c == '_' {
            let before = k > 0 && chars[k - 1].is_ascii_digit();
            let after = chars.get(k + 1).is_some_and(|d| d.is_ascii_digit());
            if !(before && after) {
                return None;
            }
        }
    }
    Some(s.replace('_', ""))
}

/// `int(text)`.
pub fn py_int(text: &str) -> Option<i64> {
    let s = without_underscores(py_strip(text))?;
    let (neg, digits) = match s.strip_prefix('-') {
        Some(d) => (true, d),
        None => (false, s.strip_prefix('+').unwrap_or(&s)),
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let v: i64 = digits.parse().ok()?;
    Some(if neg { -v } else { v })
}

/// `float(text)`.
pub fn py_float(text: &str) -> Option<f64> {
    let s = without_underscores(py_strip(text))?;
    let body = s.strip_prefix(['+', '-']).unwrap_or(&s);
    let lower = body.to_ascii_lowercase();
    if matches!(lower.as_str(), "inf" | "infinity" | "nan") {
        return s.to_ascii_lowercase().parse::<f64>().ok().or_else(|| {
            let neg = s.starts_with('-');
            Some(match lower.as_str() {
                "nan" => f64::NAN,
                _ if neg => f64::NEG_INFINITY,
                _ => f64::INFINITY,
            })
        });
    }
    if body.is_empty() || !body.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-')) {
        return None;
    }
    s.parse::<f64>().ok()
}

pub fn conv_int(s: &str) -> Result<Val, String> {
    py_int(s).map(Val::Int).ok_or_else(|| format!("invalid int value: {}", py_repr(s)))
}

pub fn conv_float(s: &str) -> Result<Val, String> {
    py_float(s).map(Val::Float).ok_or_else(|| format!("invalid float value: {}", py_repr(s)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn textwrap_breaks_on_hyphens_as_python() {
        let t = "day [YYYY-MM-DD]: that day's places and times (today by default). week [YYYY-MM-DD]: the week \
                 holding it. habits: the usual places, days and times. summary: what is kept. forget: wipe the \
                 history, the places, the things and the latest fix, after a yes. See docs/places.md.";
        assert_eq!(
            wrap(t, 78),
            vec![
                "day [YYYY-MM-DD]: that day's places and times (today by default). week [YYYY-",
                "MM-DD]: the week holding it. habits: the usual places, days and times.",
                "summary: what is kept. forget: wipe the history, the places, the things and",
                "the latest fix, after a yes. See docs/places.md.",
            ]
        );
        assert_eq!(
            wrap("Set the pet's character (replaces the retired on-device menu)", 54),
            vec!["Set the pet's character (replaces the retired on-", "device menu)"]
        );
    }

    #[test]
    fn numbers_parse_as_python() {
        assert_eq!(py_float(" 90 "), Some(90.0));
        assert_eq!(py_float("1_0"), Some(10.0));
        assert!(py_float("nan").unwrap().is_nan());
        assert_eq!(py_float("-inf"), Some(f64::NEG_INFINITY));
        assert_eq!(py_float("soon"), None);
        assert_eq!(py_float("1e3"), Some(1000.0));
        assert_eq!(py_int("-10"), Some(-10));
        assert_eq!(py_int("1.5"), None);
        assert_eq!(py_int("_1"), None);
    }

    #[test]
    fn repr_quotes_as_python() {
        assert_eq!(py_repr("web"), "'web'");
        assert_eq!(py_repr("it's"), "\"it's\"");
        assert_eq!(py_repr("a\x1b"), "'a\\x1b'");
    }
}
