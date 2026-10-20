//! `eve-bridge vault add login|card|totp`, `list`, `remove HANDLE`: Python's `vault.cli`.
//!
//! Secrets are read with getpass at the terminal: never from an argument, never from chat. The argument
//! parsing follows Python's argparse (3.13) for this parser: the same usage lines, help text (at the default
//! 80-column width), error messages, unique-prefix abbreviations (`--lab`), `--opt=value`, `--`, and exit
//! codes (0 for help, 2 for a usage error).

use std::io::{self, BufRead, Write};

use crate::{pystr, Values, Vault, VaultError, KINDS};

const PROG: &str = "eve-bridge vault";
const TOP_USAGE: &str = "usage: eve-bridge vault [-h] {add,list,remove} ...\n";
const TOP_HELP: &str = "\nSign-ins, cards and 2FA secrets eve fills for you without a model ever seeing\nthem.\n\n\
positional arguments:\n  {add,list,remove}\n    add              Save an item (values are asked for at the terminal)\n    \
list             Show the items (never their values)\n    remove           Delete an item from the vault and the Keychain\n\n\
options:\n  -h, --help         show this help message and exit\n";
const ADD_USAGE: &str = "usage: eve-bridge vault add [-h] --label LABEL [--site SITE] {login,card,totp}\n";
const ADD_HELP: &str = "\npositional arguments:\n  {login,card,totp}\n\noptions:\n  -h, --help         show this help message and exit\n  \
--label LABEL      What eve calls it, e.g. \"Chase\" or \"Visa ending 4242\"\n  \
--site SITE        login / totp: the site it fills on, e.g. chase.com\n";
const LIST_USAGE: &str = "usage: eve-bridge vault list [-h]\n";
const LIST_HELP: &str = "\noptions:\n  -h, --help  show this help message and exit\n";
const REMOVE_USAGE: &str = "usage: eve-bridge vault remove [-h] handle\n";
const REMOVE_HELP: &str = "\npositional arguments:\n  handle\n\noptions:\n  -h, --help  show this help message and exit\n";

/// One parser's fixed shape: its prog, usage and help, its value options, its single positional.
struct Parser {
    prog: &'static str,
    usage: &'static str,
    help: &'static str,
    /// (option string, dest) of every option that takes one value.
    options: &'static [(&'static str, &'static str)],
    /// The positional's name and its choices (empty: any value).
    positional: Option<(&'static str, &'static [&'static str])>,
    /// Options that must be given, by option string.
    required: &'static [&'static str],
}

const HELP_STRINGS: [&str; 2] = ["-h", "--help"];

const TOP: Parser = Parser {
    prog: PROG,
    usage: TOP_USAGE,
    help: TOP_HELP,
    options: &[],
    positional: Some(("cmd", &["add", "list", "remove"])),
    required: &[],
};
const ADD: Parser = Parser {
    prog: "eve-bridge vault add",
    usage: ADD_USAGE,
    help: ADD_HELP,
    options: &[("--label", "label"), ("--site", "site")],
    positional: Some(("kind", &KINDS)),
    required: &["--label"],
};
const LIST: Parser =
    Parser { prog: "eve-bridge vault list", usage: LIST_USAGE, help: LIST_HELP, options: &[], positional: None, required: &[] };
const REMOVE: Parser = Parser {
    prog: "eve-bridge vault remove",
    usage: REMOVE_USAGE,
    help: REMOVE_HELP,
    options: &[],
    positional: Some(("handle", &[])),
    required: &[],
};

/// How argparse left: help printed (0) or a usage error (2). Python raises SystemExit with that code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Exit(i32);

fn error(parser: &Parser, msg: &str) -> Exit {
    let mut e = io::stderr().lock();
    let _ = writeln!(e, "{}{}: error: {}", parser.usage, parser.prog, msg);
    Exit(2)
}

fn print_help(parser: &Parser) -> Exit {
    let mut o = io::stdout().lock();
    let _ = write!(o, "{}{}", parser.usage, parser.help);
    let _ = o.flush();
    Exit(0)
}

enum Arg {
    Positional,
    Help { explicit: Option<String> },
    Value { option: &'static str, dest: &'static str, explicit: Option<String> },
    Unknown,
}

fn is_negative_number(s: &str) -> bool {
    let body = &s[1..];
    let all_digits = |t: &str| !t.is_empty() && t.chars().all(|c| c.is_ascii_digit());
    all_digits(body) || body.split_once('.').is_some_and(|(a, b)| a.chars().all(|c| c.is_ascii_digit()) && all_digits(b))
}

/// argparse's `_parse_optional` for this parser's option strings.
fn classify(parser: &Parser, arg: &str) -> Result<Arg, Exit> {
    if !arg.starts_with('-') {
        return Ok(Arg::Positional);
    }
    let all: Vec<&'static str> =
        HELP_STRINGS.iter().copied().chain(parser.options.iter().map(|(o, _)| *o)).collect();
    let resolve = |option: &'static str, explicit: Option<String>| -> Arg {
        match parser.options.iter().find(|(o, _)| *o == option) {
            Some((o, dest)) => Arg::Value { option: o, dest, explicit },
            None => Arg::Help { explicit },
        }
    };
    if let Some(o) = all.iter().find(|o| **o == arg) {
        return Ok(resolve(o, None));
    }
    if arg.chars().count() == 1 {
        return Ok(Arg::Positional);
    }
    if let Some((name, value)) = arg.split_once('=') {
        if let Some(o) = all.iter().find(|o| **o == name) {
            return Ok(resolve(o, Some(value.to_string())));
        }
    }
    // _get_option_tuples: unique prefixes of long options; for `-xyz`, the short option `-x` with `yz`.
    let mut matches: Vec<(&'static str, Option<String>)> = vec![];
    let (prefix, explicit) = match arg.split_once('=') {
        Some((p, v)) => (p, Some(v.to_string())),
        None => (arg, None),
    };
    if arg.starts_with("--") {
        for o in &all {
            if o.starts_with(prefix) {
                matches.push((o, explicit.clone()));
            }
        }
    } else {
        let short: String = arg.chars().take(2).collect();
        let short_explicit: String = arg.chars().skip(2).collect();
        for o in &all {
            if *o == short {
                // `-hxyz`: argparse runs `-h` from the cluster (printing help) before it reaches `x`.
                if HELP_STRINGS.contains(o) {
                    matches.push((o, None));
                    continue;
                }
                matches.push((o, Some(short_explicit.clone())));
            } else if o.starts_with(prefix) {
                matches.push((o, explicit.clone()));
            }
        }
    }
    if matches.len() > 1 {
        let names: Vec<&str> = matches.iter().map(|(o, _)| *o).collect();
        return Err(error(parser, &format!("ambiguous option: {} could match {}", arg, names.join(", "))));
    }
    if let Some((o, explicit)) = matches.pop() {
        return Ok(resolve(o, explicit));
    }
    if is_negative_number(arg) || arg.contains(' ') {
        return Ok(Arg::Positional);
    }
    Ok(Arg::Unknown)
}

/// What one parser collected.
#[derive(Default, Debug)]
struct Parsed {
    positional: Option<String>,
    values: Vec<(&'static str, String)>,
    /// For the top parser: the arguments after the command, for the command's own parser.
    rest: Vec<String>,
    extras: Vec<String>,
}

impl Parsed {
    fn value(&self, dest: &str) -> Option<&str> {
        self.values.iter().rev().find(|(d, _)| *d == dest).map(|(_, v)| v.as_str())
    }
}

fn choice_error(parser: &Parser, name: &str, value: &str, choices: &[&str]) -> Exit {
    let choices: Vec<String> = choices.iter().map(|c| pystr::repr(c)).collect();
    error(parser, &format!("argument {name}: invalid choice: {} (choose from {})", pystr::repr(value), choices.join(", ")))
}

/// argparse's `parse_known_args` for one parser. `subcommand`: the positional takes every argument after it
/// (argparse's `A...`), which the top parser hands to the command's parser.
fn parse(parser: &Parser, argv: &[String], subcommand: bool) -> Result<Parsed, Exit> {
    // argparse classifies every argument before acting on any: `--` and everything after it is positional.
    let mut kinds: Vec<Option<Arg>> = Vec::with_capacity(argv.len());
    let mut after_dashes = false;
    for arg in argv {
        if after_dashes {
            kinds.push(Some(Arg::Positional));
        } else if arg == "--" {
            after_dashes = true;
            kinds.push(None);
        } else {
            kinds.push(Some(classify(parser, arg)?));
        }
    }
    let mut out = Parsed::default();
    let mut i = 0;
    while i < argv.len() {
        let arg = &argv[i];
        let Some(kind) = kinds[i].take() else {
            i += 1;
            continue;
        };
        i += 1;
        match kind {
            Arg::Positional => match parser.positional {
                Some((name, choices)) if out.positional.is_none() => {
                    if !choices.is_empty() && !choices.contains(&arg.as_str()) {
                        return Err(choice_error(parser, name, arg, choices));
                    }
                    out.positional = Some(arg.clone());
                    if subcommand {
                        out.rest = argv[i..].to_vec();
                        break;
                    }
                }
                _ => out.extras.push(arg.clone()),
            },
            Arg::Help { explicit: Some(x) } => {
                return Err(error(parser, &format!("argument -h/--help: ignored explicit argument {}", pystr::repr(&x))));
            }
            Arg::Help { explicit: None } => return Err(print_help(parser)),
            Arg::Value { dest, explicit: Some(v), .. } => out.values.push((dest, v)),
            Arg::Value { option, dest, explicit: None } => match kinds.get(i) {
                Some(Some(Arg::Positional)) => {
                    out.values.push((dest, argv[i].clone()));
                    kinds[i] = None;
                    i += 1;
                }
                _ => return Err(error(parser, &format!("argument {option}: expected one argument"))),
            },
            Arg::Unknown => out.extras.push(arg.clone()),
        }
    }
    let mut missing: Vec<&str> = vec![];
    if let Some((name, _)) = parser.positional {
        if out.positional.is_none() {
            missing.push(name);
        }
    }
    for req in parser.required {
        let dest = parser.options.iter().find(|(o, _)| o == req).map_or("", |(_, d)| *d);
        if out.value(dest).is_none() {
            missing.push(req);
        }
    }
    if !missing.is_empty() {
        return Err(error(parser, &format!("the following arguments are required: {}", missing.join(", "))));
    }
    Ok(out)
}

enum Command {
    Add { kind: String, label: String, site: String },
    List,
    Remove { handle: String },
}

fn parse_args(argv: &[String]) -> Result<Command, Exit> {
    let top = parse(&TOP, argv, true)?;
    let cmd = top.positional.clone().unwrap_or_default();
    let sub = match cmd.as_str() {
        "add" => &ADD,
        "list" => &LIST,
        _ => &REMOVE,
    };
    let parsed = parse(sub, &top.rest, false)?;
    let extras: Vec<String> = top.extras.iter().chain(parsed.extras.iter()).cloned().collect();
    if !extras.is_empty() {
        return Err(error(&TOP, &format!("unrecognized arguments: {}", extras.join(" "))));
    }
    Ok(match cmd.as_str() {
        "add" => Command::Add {
            kind: parsed.positional.clone().unwrap_or_default(),
            label: parsed.value("label").unwrap_or_default().to_string(),
            site: parsed.value("site").unwrap_or_default().to_string(),
        },
        "list" => Command::List,
        _ => Command::Remove { handle: parsed.positional.clone().unwrap_or_default() },
    })
}

/// A prompt answered at the terminal; an error (EOF) ends the command as Python's EOFError would.
pub type Ask<'a> = &'a mut dyn FnMut(&str) -> io::Result<String>;

/// `eve-bridge vault ...` with the given vault (None: `Vault()`), prompts and output. Returns the exit
/// code: 0 done, 1 refused or failed, 2 a usage error (where Python raises SystemExit(2)).
pub fn cli(argv: &[String], vault: Option<&Vault>, ask: Ask<'_>, ask_secret: Ask<'_>, out: &mut dyn Write) -> i32 {
    let command = match parse_args(argv) {
        Ok(c) => c,
        Err(Exit(code)) => return code,
    };
    let owned;
    let v = match vault {
        Some(v) => v,
        None => match Vault::open_default() {
            Ok(v) => {
                owned = v;
                &owned
            }
            Err(e) => return fail(&e),
        },
    };
    let io_fail = |e: io::Error| fail(&VaultError::Os(e.to_string()));
    match command {
        Command::List => {
            let items = v.items();
            let text = if items.is_empty() {
                "The vault is empty.".to_string()
            } else {
                items.iter().map(|i| i.line()).collect::<Vec<_>>().join("\n")
            };
            let _ = writeln!(out, "{text}");
            0
        }
        Command::Remove { handle } => match v.remove(&handle) {
            Ok(ok) => {
                let _ = if ok { writeln!(out, "Removed.") } else { writeln!(out, "No item {}.", pystr::repr(&handle)) };
                if ok {
                    0
                } else {
                    1
                }
            }
            Err(e) => fail(&e),
        },
        Command::Add { kind, label, site } => {
            if (kind == "login" || kind == "totp") && site.is_empty() {
                return error(&TOP, &format!("a {kind} needs --site")).0;
            }
            let mut value = Values::new();
            let result: io::Result<()> = (|| {
                if kind == "login" {
                    let username = pystr::strip(&ask("Username: ")?).to_string();
                    value.insert("username".into(), username.into());
                    value.insert("password".into(), ask_secret("Password: ")?.into());
                } else if kind == "card" {
                    let exp = pystr::strip(&ask("Expiry (MM/YY): ")?).to_string();
                    let (month, year) = exp.split_once('/').unwrap_or((exp.as_str(), ""));
                    let (month, year) = (pystr::strip(month).to_string(), pystr::strip(year).to_string());
                    let number = ask_secret("Card number: ")?;
                    value.insert("number".into(), number.into());
                    value.insert("exp_month".into(), month.into());
                    value.insert("exp_year".into(), year.into());
                    value.insert("cvc".into(), ask_secret("CVC: ")?.into());
                    value.insert("name".into(), pystr::strip(&ask("Name on card: ")?).to_string().into());
                } else {
                    value.insert("secret".into(), ask_secret("2FA secret or otpauth:// URI: ")?.into());
                }
                Ok(())
            })();
            if let Err(e) = result {
                return io_fail(e);
            }
            match v.add(&kind, &label, &value, &site) {
                Ok(item) => {
                    let _ = writeln!(out, "Saved {}", item.line());
                    0
                }
                Err(e) => fail(&e),
            }
        }
    }
}

fn fail(e: &VaultError) -> i32 {
    let _ = writeln!(io::stderr().lock(), "vault: {e}");
    1
}

/// `cli(argv)` with Python's defaults: `input()` and `getpass.getpass()` at the terminal, stdout.
pub fn cli_main(argv: &[String]) -> i32 {
    let mut stdout = io::stdout();
    cli(argv, None, &mut input, &mut getpass, &mut stdout)
}

/// Python's `input(prompt)`: the prompt on stdout, one line from stdin without its newline; EOF is an error.
pub fn input(prompt: &str) -> io::Result<String> {
    {
        let mut o = io::stdout().lock();
        o.write_all(prompt.as_bytes())?;
        o.flush()?;
    }
    let mut line = String::new();
    if io::stdin().lock().read_line(&mut line)? == 0 {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "EOF when reading a line"));
    }
    if line.ends_with('\n') {
        line.pop();
    }
    Ok(line)
}

/// Python's `getpass.getpass(prompt)`: the prompt and the reply on the controlling terminal with echo off;
/// without a terminal, a warning and a plain read from stdin.
pub fn getpass(prompt: &str) -> io::Result<String> {
    #[cfg(unix)]
    {
        if let Ok(tty) = std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty") {
            return getpass_tty(tty, prompt);
        }
    }
    {
        let mut e = io::stderr().lock();
        let _ = writeln!(e, "Warning: Password input may be echoed.");
        let _ = e.write_all(prompt.as_bytes());
        let _ = e.flush();
    }
    let mut line = String::new();
    if io::stdin().lock().read_line(&mut line)? == 0 {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "EOF"));
    }
    if line.ends_with('\n') {
        line.pop();
    }
    Ok(line)
}

#[cfg(unix)]
fn getpass_tty(tty: std::fs::File, prompt: &str) -> io::Result<String> {
    use std::io::Read;
    use std::os::unix::io::AsRawFd;
    let fd = tty.as_raw_fd();
    // SAFETY: `fd` is an open terminal for the life of `tty`; termios is plain data.
    let mut old: libc::termios = unsafe { std::mem::zeroed() };
    let have_termios = unsafe { libc::tcgetattr(fd, &mut old) } == 0;
    let mut writer = &tty;
    writer.write_all(prompt.as_bytes())?;
    writer.flush()?;
    if have_termios {
        let mut new = old;
        new.c_lflag &= !libc::ECHO;
        // SAFETY: as above.
        unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &new) };
    }
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    let mut reader = &tty;
    let read = loop {
        match reader.read(&mut byte) {
            Ok(0) => break Ok(()),
            Ok(_) if byte[0] == b'\n' => break Ok(()),
            Ok(_) => line.push(byte[0]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => break Err(e),
        }
    };
    if have_termios {
        // SAFETY: as above.
        unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &old) };
    }
    let _ = writer.write_all(b"\n");
    let _ = writer.flush();
    read?;
    if line.is_empty() && byte[0] != b'\n' {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "EOF"));
    }
    Ok(String::from_utf8_lossy(&line).into_owned())
}
