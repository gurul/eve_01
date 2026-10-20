//! `eve-bridge app new|list`: Python's argparse parser for it, reproduced (Python 3.13's parsing, messages and the
//! 80-column help it prints), and the command itself.

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

use crate::{find_root, scaffold, ScaffoldError};

const MAIN_USAGE: &str = "usage: eve-bridge app [-h] {new,list} ...\n";
const MAIN_HELP: &str = "usage: eve-bridge app [-h] {new,list} ...

eve's apps: the owner's picks, and yours.

positional arguments:
  {new,list}
    new       write a new app: a module, its test, its doc and its registry
              line
    list      every app: its slot, its switch, whether the switch is on here,
              its doc

options:
  -h, --help  show this help message and exit
";
const NEW_USAGE: &str = "usage: eve-bridge app new [-h] [--dry-run] name\n";
const NEW_HELP: &str = "usage: eve-bridge app new [-h] [--dry-run] name

positional arguments:
  name        2 to 16 lowercase letters or digits, starting with a letter
              (e.g. fan)

options:
  -h, --help  show this help message and exit
  --dry-run   say what would be written; write nothing
";
const LIST_USAGE: &str = "usage: eve-bridge app list [-h]\n";
const LIST_HELP: &str = "usage: eve-bridge app list [-h]

options:
  -h, --help  show this help message and exit
";

#[derive(Clone, Copy, PartialEq, Debug)]
enum Act {
    Help,
    DryRun,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Pos {
    /// `name`: one argument
    Name,
    /// the subcommand: argparse's PARSER (`A...`)
    Cmd,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Which {
    Main,
    New,
    List,
}

struct Parser {
    which: Which,
    prog: &'static str,
    usage: &'static str,
    help: &'static str,
    /// `_option_string_actions`, in insertion order
    options: &'static [(&'static str, Act)],
    positionals: &'static [Pos],
}

const MAIN: Parser = Parser {
    which: Which::Main,
    prog: "eve-bridge app",
    usage: MAIN_USAGE,
    help: MAIN_HELP,
    options: &[("-h", Act::Help), ("--help", Act::Help)],
    positionals: &[Pos::Cmd],
};
const NEW: Parser = Parser {
    which: Which::New,
    prog: "eve-bridge app new",
    usage: NEW_USAGE,
    help: NEW_HELP,
    options: &[("-h", Act::Help), ("--help", Act::Help), ("--dry-run", Act::DryRun)],
    positionals: &[Pos::Name],
};
const LIST: Parser = Parser {
    which: Which::List,
    prog: "eve-bridge app list",
    usage: LIST_USAGE,
    help: LIST_HELP,
    options: &[("-h", Act::Help), ("--help", Act::Help)],
    positionals: &[],
};

/// What argparse does instead of returning: print help (exit 0) or a usage and an error (exit 2).
#[derive(Debug, PartialEq)]
enum Stop {
    Help(Which),
    Error(Which, String),
}

#[derive(Debug, Default, PartialEq)]
pub(crate) struct Args {
    pub cmd: Option<String>,
    pub name: Option<String>,
    pub dry_run: bool,
}

/// `(action, option_string, sep, explicit_arg)`; action None is an unknown optional.
type OptTuple = (Option<Act>, String, Option<String>, Option<String>);

static NEGATIVE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:-\d+|-\d*\.\d+)\n?\z").expect("valid negative-number regex"));

fn partition(s: &str) -> (String, Option<String>, Option<String>) {
    match s.split_once('=') {
        Some((a, b)) => (a.to_string(), Some("=".to_string()), Some(b.to_string())),
        None => (s.to_string(), None, None),
    }
}

impl Parser {
    fn action(&self, option: &str) -> Option<Act> {
        self.options.iter().find(|(o, _)| *o == option).map(|(_, a)| *a)
    }

    fn option_names(&self, act: Act) -> String {
        self.options.iter().filter(|(_, a)| *a == act).map(|(o, _)| *o).collect::<Vec<_>>().join("/")
    }

    fn parse_optional(&self, arg: &str) -> Option<Vec<OptTuple>> {
        if arg.is_empty() || !arg.starts_with('-') {
            return None;
        }
        if let Some(act) = self.action(arg) {
            return Some(vec![(Some(act), arg.to_string(), None, None)]);
        }
        if arg.chars().count() == 1 {
            return None;
        }
        let (option, sep, explicit) = partition(arg);
        if sep.is_some() {
            if let Some(act) = self.action(&option) {
                return Some(vec![(Some(act), option, sep, explicit)]);
            }
        }
        let tuples = self.option_tuples(arg);
        if !tuples.is_empty() {
            return Some(tuples);
        }
        if NEGATIVE.is_match(arg) {
            return None;
        }
        if arg.contains(' ') {
            return None;
        }
        Some(vec![(None, arg.to_string(), None, None)])
    }

    fn option_tuples(&self, arg: &str) -> Vec<OptTuple> {
        let mut out = Vec::new();
        let second = arg.chars().nth(1);
        let (prefix, sep, explicit) = partition(arg);
        if second == Some('-') {
            for (option, act) in self.options {
                if option.starts_with(&prefix) {
                    out.push((Some(*act), option.to_string(), sep.clone(), explicit.clone()));
                }
            }
        } else {
            let short: String = arg.chars().take(2).collect();
            let short_explicit: String = arg.chars().skip(2).collect();
            for (option, act) in self.options {
                if *option == short {
                    out.push((Some(*act), option.to_string(), Some(String::new()), Some(short_explicit.clone())));
                } else if option.starts_with(&prefix) {
                    out.push((Some(*act), option.to_string(), sep.clone(), explicit.clone()));
                }
            }
        }
        out
    }

    fn nargs_pattern(pos: Pos) -> &'static str {
        match pos {
            Pos::Name => "(-*A-*)",
            Pos::Cmd => "(-*A[-AO]*)",
        }
    }

    /// `_match_arguments_partial`
    fn match_partial(positionals: &[Pos], pattern: &str) -> Vec<usize> {
        for i in (1..=positionals.len()).rev() {
            let re: String = positionals[..i].iter().map(|p| Self::nargs_pattern(*p)).collect();
            let re = Regex::new(&format!("^{re}")).expect("valid nargs pattern");
            if let Some(caps) = re.captures(pattern) {
                let mut result: Vec<usize> = (1..caps.len()).map(|g| caps.get(g).map_or(0, |m| m.len())).collect();
                let end = caps.get(0).map_or(0, |m| m.end());
                if end < pattern.len() && pattern.as_bytes()[end] == b'O' {
                    while result.last() == Some(&0) {
                        result.pop();
                    }
                }
                return result;
            }
        }
        Vec::new()
    }

    /// `parse_known_args`: the namespace and the extras (a subparser's included), or where argparse stops.
    fn parse_known(&self, args: &[String], ns: &mut Args) -> Result<Vec<String>, Stop> {
        let mut sub_extras = Vec::new();
        let mut extras = self.parse_inner(args, ns, &mut sub_extras).map_err(|e| match e {
            Inner::Stop(s) => s,
            Inner::Arg(msg) => Stop::Error(self.which, msg),
        })?;
        extras.extend(sub_extras);
        Ok(extras)
    }

    fn parse_inner(&self, args: &[String], ns: &mut Args, sub_extras: &mut Vec<String>) -> Result<Vec<String>, Inner> {
        let mut indices: HashMap<usize, Vec<OptTuple>> = HashMap::new();
        let mut pattern = String::new();
        let mut it = args.iter().enumerate();
        while let Some((i, arg)) = it.next() {
            if arg == "--" {
                pattern.push('-');
                for _ in it.by_ref() {
                    pattern.push('A');
                }
                break;
            }
            match self.parse_optional(arg) {
                None => pattern.push('A'),
                Some(t) => {
                    indices.insert(i, t);
                    pattern.push('O');
                }
            }
        }
        let pat = pattern.as_bytes();
        let mut positionals: Vec<Pos> = self.positionals.to_vec();
        let mut seen: Vec<Pos> = Vec::new();
        let mut extras: Vec<String> = Vec::new();

        let max_option = indices.keys().max().copied();
        let mut start = 0usize;
        if let Some(max_option) = max_option {
            while start <= max_option {
                let mut next = start;
                while next <= max_option && !indices.contains_key(&next) {
                    next += 1;
                }
                if start != next {
                    let end = self.consume_positionals(start, args, pat, &mut positionals, &mut seen, ns, sub_extras)?;
                    if end > start {
                        start = end;
                        continue;
                    }
                    start = end;
                }
                if !indices.contains_key(&start) {
                    extras.extend(args[start..next].iter().cloned());
                    start = next;
                }
                start = self.consume_optional(start, args, &indices, &mut extras, ns)?;
            }
        }
        let stop = self.consume_positionals(start, args, pat, &mut positionals, &mut seen, ns, sub_extras)?;
        extras.extend(args[stop..].iter().cloned());

        if self.positionals.contains(&Pos::Name) && !seen.contains(&Pos::Name) {
            return Err(Inner::Arg("the following arguments are required: name".to_string()));
        }
        Ok(extras)
    }

    fn consume_optional(
        &self,
        start: usize,
        args: &[String],
        indices: &HashMap<usize, Vec<OptTuple>>,
        extras: &mut Vec<String>,
        ns: &mut Args,
    ) -> Result<usize, Inner> {
        let tuples = &indices[&start];
        if tuples.len() > 1 {
            let options: Vec<&str> = tuples.iter().map(|t| t.1.as_str()).collect();
            return Err(Inner::Arg(format!("ambiguous option: {} could match {}", args[start], options.join(", "))));
        }
        let (mut action, mut option, mut sep, mut explicit) = tuples[0].clone();
        let mut acts: Vec<Act> = Vec::new();
        let stop;
        loop {
            let Some(act) = action else {
                extras.push(args[start].clone());
                return Ok(start + 1);
            };
            if let Some(exp) = explicit.clone() {
                // every option here takes no argument (help, store_true): arg_count is 0
                let single = option.chars().nth(1).is_some_and(|c| c != '-');
                if single && !exp.is_empty() {
                    if sep.as_deref().is_some_and(|s| !s.is_empty()) || exp.starts_with('-') {
                        return Err(self.ignored(act, &exp));
                    }
                    acts.push(act);
                    let first = exp.chars().next().unwrap_or_default();
                    let lead = option.chars().next().unwrap_or('-');
                    option = format!("{lead}{first}");
                    if let Some(next) = self.action(&option) {
                        action = Some(next);
                        let rest: String = exp.chars().skip(1).collect();
                        if rest.is_empty() {
                            sep = None;
                            explicit = None;
                        } else if let Some(r) = rest.strip_prefix('=') {
                            sep = Some("=".to_string());
                            explicit = Some(r.to_string());
                        } else {
                            sep = Some(String::new());
                            explicit = Some(rest);
                        }
                    } else {
                        extras.push(format!("{lead}{exp}"));
                        stop = start + 1;
                        break;
                    }
                } else {
                    return Err(self.ignored(act, &exp));
                }
            } else {
                acts.push(act);
                stop = start + 1;
                break;
            }
        }
        for act in acts {
            match act {
                Act::Help => return Err(Inner::Stop(Stop::Help(self.which))),
                Act::DryRun => ns.dry_run = true,
            }
        }
        Ok(stop)
    }

    fn ignored(&self, act: Act, explicit: &str) -> Inner {
        Inner::Arg(format!(
            "argument {}: ignored explicit argument {}",
            self.option_names(act),
            eve_app_registry::py_str_repr(explicit)
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn consume_positionals(
        &self,
        start: usize,
        args: &[String],
        pat: &[u8],
        positionals: &mut Vec<Pos>,
        seen: &mut Vec<Pos>,
        ns: &mut Args,
        sub_extras: &mut Vec<String>,
    ) -> Result<usize, Inner> {
        let selected = std::str::from_utf8(&pat[start.min(pat.len())..]).unwrap_or("");
        let counts = Self::match_partial(positionals, selected);
        let mut index = start;
        for (pos, count) in positionals.iter().copied().zip(counts.iter().copied()) {
            let mut taken: Vec<String> = args[index..index + count].to_vec();
            match pos {
                Pos::Cmd => {
                    if pat.get(index) == Some(&b'-') {
                        taken.remove(0);
                    }
                }
                Pos::Name => {
                    if pat[index..index + count].contains(&b'-') {
                        if let Some(k) = taken.iter().position(|a| a == "--") {
                            taken.remove(k);
                        }
                    }
                }
            }
            index += count;
            seen.push(pos);
            match pos {
                Pos::Name => ns.name = taken.into_iter().next(),
                Pos::Cmd => self.take_cmd(&taken, ns, sub_extras)?,
            }
        }
        let n = counts.len().min(positionals.len());
        positionals.drain(..n);
        Ok(index)
    }

    fn take_cmd(&self, values: &[String], ns: &mut Args, sub_extras: &mut Vec<String>) -> Result<(), Inner> {
        let name = values.first().cloned().unwrap_or_default();
        let sub = match name.as_str() {
            "new" => &NEW,
            "list" => &LIST,
            _ => {
                return Err(Inner::Arg(format!(
                    "argument cmd: invalid choice: {} (choose from 'new', 'list')",
                    eve_app_registry::py_str_repr(&name)
                )))
            }
        };
        ns.cmd = Some(name);
        let extras = sub.parse_known(&values[1..], ns).map_err(Inner::Stop)?;
        sub_extras.extend(extras);
        Ok(())
    }
}

enum Inner {
    Stop(Stop),
    Arg(String),
}

fn parser_for(which: Which) -> &'static Parser {
    match which {
        Which::Main => &MAIN,
        Which::New => &NEW,
        Which::List => &LIST,
    }
}

/// `parser.parse_args(argv)`: the arguments, or the exit code after argparse printed help or an error.
pub(crate) fn parse_args(argv: &[String], out: &mut dyn Write, err: &mut dyn Write) -> Result<Args, i32> {
    let mut ns = Args::default();
    let stop = match MAIN.parse_known(argv, &mut ns) {
        Ok(extras) if extras.is_empty() => return Ok(ns),
        Ok(extras) => Stop::Error(Which::Main, format!("unrecognized arguments: {}", extras.join(" "))),
        Err(stop) => stop,
    };
    match stop {
        Stop::Help(which) => {
            let _ = out.write_all(parser_for(which).help.as_bytes());
            Err(0)
        }
        Stop::Error(which, msg) => {
            let p = parser_for(which);
            let _ = err.write_all(p.usage.as_bytes());
            let _ = writeln!(err, "{}: error: {msg}", p.prog);
            Err(2)
        }
    }
}

/// `cli(argv)` with its world passed in: `cwd` is the working folder `find_root` starts from, `environ` the
/// environment `app list` reads the switches from (None: the process's).
pub fn cli_in(
    argv: &[String],
    cwd: &Path,
    environ: Option<&HashMap<String, String>>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let args = match parse_args(argv, out, err) {
        Ok(a) => a,
        Err(code) => return code,
    };
    if args.cmd.as_deref() == Some("list") {
        for app in eve_app_registry::APPS.iter() {
            let on = if eve_app_registry::switched_on(app, environ) { "on" } else { "off" };
            let _ = writeln!(out, "{:<10} {:<14} {:<4} {}", app.slot, app.switch, on, app.doc);
        }
        return 0;
    }
    if args.cmd.as_deref() != Some("new") {
        let _ = out.write_all(MAIN_HELP.as_bytes());
        return if argv.is_empty() { 0 } else { 2 };
    }
    let name = args.name.unwrap_or_default();
    let (root, wrote) = match find_root(Some(cwd)).and_then(|root| {
        let wrote = scaffold(&name, Some(&root), args.dry_run)?;
        Ok((root, wrote))
    }) {
        Ok(r) => r,
        Err(ScaffoldError::Refused(e)) => {
            let _ = writeln!(err, "Not written: {e}.");
            return 1;
        }
        Err(e) => {
            // Python lets anything else propagate: a traceback, exit status 1
            let _ = writeln!(err, "eve-bridge app: {e}");
            return 1;
        }
    };
    let verb = if args.dry_run { "Would write" } else { "Wrote" };
    for path in &wrote {
        let rel = path.strip_prefix(&root).unwrap_or(path);
        let _ = writeln!(out, "{verb} {}", rel.display());
    }
    if args.dry_run {
        return 0;
    }
    let switch = format!("EVE_{}", name.to_uppercase());
    let _ = writeln!(
        out,
        "
Next:
  1. Run its tests: uv run --project bridge --extra dev pytest -q -p no:cacheprovider -c bridge/pyproject.toml \\
       --rootdir bridge bridge/tests/test_{name}.py
  2. Fill in the device code in bridge/src/eve_bridge/{name}.py (the comment in its __init__ says where), and
     set symbol and detail in its registry line. Changing its tools? See \"Change the tools\" in the walk-through.
  3. Run the whole suite and lint (AGENTS.md, \"Working on the code\"); a new app needs no edit to other tests.
  4. Turn it on, once it is merged into the branch the daemon runs from (the main checkout; AGENTS.md rule 5):
     add {switch}=1 to ~/.config/eve-bridge/env, then restart the daemon:
       launchctl kickstart -k gui/$(id -u)/com.github.eve-bridge.daemon
  5. If it will stay on, pre-register a bar and run a blind intent_eval set first (AGENTS.md rule 6).
See docs/adding-an-app.md."
    );
    0
}

/// `cli(argv)`: `eve-bridge app new <name> [--dry-run]` and `eve-bridge app list`, from the working folder, with the
/// process's environment, stdout and stderr. The exit status.
pub fn cli(argv: &[String]) -> i32 {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let (mut out, mut err) = (std::io::stdout().lock(), std::io::stderr().lock());
    cli_in(argv, &cwd, None, &mut out, &mut err)
}
