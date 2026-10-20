//! `--help` for eve's apps' own command lines (lights, spotify, firetv, obsbot/cam) in argparse's layout.
//!
//! Python's apps parse with argparse, so `eve-bridge lights --help` prints argparse's usage and sections. The Rust
//! apps parse with clap. Their clap definition ([`eve_lights::cli::clap_command`] and the others) is turned into an
//! [`argparse::Parser`] here, and its help is printed when the command line asks for help; everything else still
//! goes to the app's own `cli`.

use clap::{ArgAction, Command};

use crate::argparse::{self, Arg, Parser};

/// The app's clap definition, by registry slot.
pub fn clap_command(slot: &str) -> Option<Command> {
    match slot {
        "lights" => Some(eve_lights::cli::clap_command()),
        "spotify" => Some(eve_spotify::clap_command()),
        "firetv" => Some(eve_firetv::clap_command()),
        "obsbot" => Some(eve_obsbot::clap_command()),
        _ => None,
    }
}

fn text(s: Option<&clap::builder::StyledStr>) -> Option<String> {
    s.map(|t| t.to_string()).filter(|t| !t.is_empty())
}

/// One clap argument as argparse declares it.
fn arg_of(a: &clap::Arg) -> Option<Arg> {
    if a.is_hide_set() {
        return None;
    }
    let choices: Vec<String> = a.get_possible_values().iter().map(|v| v.get_name().to_string()).collect();
    let flag = matches!(a.get_action(), ArgAction::SetTrue | ArgAction::SetFalse | ArgAction::Count);
    let range = a.get_num_args();
    let mut out = if a.is_positional() {
        let mut p = Arg::pos(&a.get_id().to_string().replace('-', "_"));
        match range {
            Some(r) if r.max_values() == usize::MAX && r.min_values() == 0 => p = p.zero_or_more(),
            Some(r) if r.max_values() == usize::MAX => p = p.one_or_more(),
            Some(r) if r.max_values() == 1 && r.min_values() == 0 => p = p.optional(),
            _ if !a.is_required_set() => p = p.optional(),
            _ => {}
        }
        p
    } else {
        let mut options: Vec<String> = Vec::new();
        if let Some(c) = a.get_short() {
            options.push(format!("-{c}"));
        }
        if let Some(l) = a.get_long() {
            options.push(format!("--{l}"));
        }
        if options.is_empty() {
            return None;
        }
        let refs: Vec<&str> = options.iter().map(String::as_str).collect();
        let mut o = Arg::opt(&refs);
        if flag {
            o = o.flag();
        }
        if a.is_required_set() {
            o = o.required();
        }
        o
    };
    if !flag && !choices.is_empty() {
        let refs: Vec<&str> = choices.iter().map(String::as_str).collect();
        out = out.choices(&refs);
    }
    if let Some(names) = a.get_value_names() {
        let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        out = out.metavar(&refs);
    }
    if let Some(h) = text(a.get_help()) {
        out = out.help(&h);
    }
    Some(out)
}

/// The clap command as an argparse parser named `prog`, its subcommands as `prog <name>`. `top`: the app's own
/// parser, whose `about` is its description (a subcommand's `about` is its line in the parent's list).
pub fn parser_of(cmd: &Command, prog: &str, top: bool) -> Parser {
    let description = if top { text(cmd.get_long_about()).or_else(|| text(cmd.get_about())) } else { None };
    let mut p = Parser::new(prog, description.as_deref(), !cmd.is_disable_help_flag_set());
    for a in cmd.get_arguments() {
        if let Some(arg) = arg_of(a) {
            p.arg(arg);
        }
    }
    let subs: Vec<&Command> = cmd.get_subcommands().filter(|s| !s.is_hide_set()).collect();
    if !subs.is_empty() {
        let built: Vec<(String, Vec<String>, Option<String>, Parser)> = subs
            .iter()
            .map(|s| {
                let name = s.get_name().to_string();
                let aliases: Vec<String> = s.get_visible_aliases().map(str::to_string).collect();
                (name.clone(), aliases, text(s.get_about()), parser_of(s, &format!("{prog} {name}"), false))
            })
            .collect();
        p.subparsers("cmd", Vec::new());
        if let Some(argparse::Arg { kind: argparse::Kind::Subparsers(sp), .. }) = p.args.last_mut() {
            sp.choices = built
                .into_iter()
                .map(|(name, aliases, help, parser)| argparse::Choice { name, aliases, help, parser })
                .collect();
        }
    }
    p
}

/// Whether `word` names `option` the way argparse takes it: whole, or as a unique prefix of a long option.
fn names_option(word: &str, options: &[String], all_long: &[String]) -> bool {
    if options.iter().any(|o| o == word) {
        return true;
    }
    if word.starts_with("--") && word.len() > 2 {
        let hits: Vec<&String> = all_long.iter().filter(|o| o.starts_with(word)).collect();
        return hits.len() == 1 && options.contains(hits[0]);
    }
    false
}

/// The subcommand path whose help `argv` asks for, as argparse would reach it: `-h`/`--help` (or a unique prefix
/// of `--help`) at some level, after that level's options and before anything that ends the walk. None: no help is
/// asked for, or the command line goes wrong first (the app's own parser then says why).
pub fn help_path(parser: &Parser, argv: &[String]) -> Option<Vec<String>> {
    let mut path: Vec<String> = Vec::new();
    let mut level = parser;
    let mut leaf_positionals = 0;
    let mut i = 0;
    while i < argv.len() {
        let word = &argv[i];
        if word == "--" {
            return None;
        }
        let long: Vec<String> = level.args.iter().flat_map(|a| a.options.iter().filter(|o| o.starts_with("--")).cloned()).collect();
        if word.starts_with('-') && word.len() > 1 && !is_negative_number(word) {
            let (head, has_value) = match word.split_once('=') {
                Some((h, _)) if h.starts_with("--") => (h.to_string(), true),
                _ => (word.clone(), false),
            };
            let Some(a) = level.args.iter().find(|a| names_option(&head, &a.options, &long)) else {
                return None; // unknown here: the app's parser says so
            };
            if matches!(a.kind, argparse::Kind::Help) {
                return Some(path);
            }
            let takes_value = !matches!(a.kind, argparse::Kind::StoreTrue);
            i += if takes_value && !has_value { 2 } else { 1 };
            continue;
        }
        let subs = level.args.iter().find_map(|a| match &a.kind {
            argparse::Kind::Subparsers(s) => Some(s),
            _ => None,
        });
        match subs {
            Some(s) => {
                let choice = s.choices.iter().find(|c| &c.name == word || c.aliases.iter().any(|al| al == word))?;
                path.push(choice.name.clone());
                level = &choice.parser;
            }
            None => {
                // a positional of the leaf: help may still follow, unless argparse turns the word down first (a
                // choice it does not offer is an error before a later -h is reached)
                let positional = level
                    .args
                    .iter()
                    .filter(|a| a.options.is_empty() && !matches!(a.kind, argparse::Kind::Subparsers(_)))
                    .nth(leaf_positionals);
                if let Some(choices) = positional.and_then(|a| a.choices.as_ref()) {
                    if !choices.iter().any(|c| c == word) {
                        return None;
                    }
                }
                leaf_positionals += 1;
            }
        }
        i += 1;
    }
    None
}

fn is_negative_number(word: &str) -> bool {
    word.strip_prefix('-').is_some_and(|rest| !rest.is_empty() && rest.parse::<f64>().is_ok())
}

/// argparse's help for the app in `slot`, when `argv` (the words after the app's name) asks for it.
pub fn help_for(slot: &str, argv: &[String]) -> Option<String> {
    let cmd = clap_command(slot)?;
    let prog = if cmd.get_name().starts_with("eve-bridge") { cmd.get_name().to_string() } else { format!("eve-bridge {slot}") };
    let parser = parser_of(&cmd, &prog, true);
    let path = help_path(&parser, argv)?;
    let refs: Vec<&str> = path.iter().map(String::as_str).collect();
    parser.find(&refs).map(Parser::format_help)
}

/// app_template's parser (`eve-bridge app new|list`), as argparse declares it. eve-app-template prints its help and
/// usage at 80 columns only; this one wraps at the terminal's width, as argparse does.
pub fn app_template_parser() -> Parser {
    let prog = "eve-bridge app";
    let mut parser = Parser::new(prog, Some("eve's apps: the owner's picks, and yours."), true);
    let mut new = Parser::new(&format!("{prog} new"), None, true);
    new.arg(Arg::pos("name").help("2 to 16 lowercase letters or digits, starting with a letter (e.g. fan)"))
        .arg(Arg::opt(&["--dry-run"]).flag().help("say what would be written; write nothing"));
    parser.subparsers(
        "cmd",
        vec![
            ("new", &[], Some("write a new app: a module, its test, its doc and its registry line"), new),
            (
                "list",
                &[],
                Some("every app: its slot, its switch, whether the switch is on here, its doc"),
                Parser::new(&format!("{prog} list"), None, true),
            ),
        ],
    );
    parser
}

/// vault's parser (`eve-bridge vault add|list|remove`), as argparse declares it. eve-vault prints its help and usage
/// at 80 columns only; this one wraps at the terminal's width, as argparse does.
pub fn vault_parser() -> Parser {
    let prog = "eve-bridge vault";
    let mut parser = Parser::new(
        prog,
        Some("Sign-ins, cards and 2FA secrets eve fills for you without a model ever seeing them."),
        true,
    );
    let mut add = Parser::new(&format!("{prog} add"), None, true);
    let kinds: Vec<&str> = eve_vault::KINDS.to_vec();
    add.arg(Arg::pos("kind").choices(&kinds))
        .arg(Arg::opt(&["--label"]).required().help("What eve calls it, e.g. \"Chase\" or \"Visa ending 4242\""))
        .arg(Arg::opt(&["--site"]).default_str("").help("login / totp: the site it fills on, e.g. chase.com"));
    let mut remove = Parser::new(&format!("{prog} remove"), None, true);
    remove.arg(Arg::pos("handle"));
    parser.subparsers(
        "cmd",
        vec![
            ("add", &[], Some("Save an item (values are asked for at the terminal)"), add),
            ("list", &[], Some("Show the items (never their values)"), Parser::new(&format!("{prog} list"), None, true)),
            ("remove", &[], Some("Delete an item from the vault and the Keychain"), remove),
        ],
    );
    // add_subparsers(dest="cmd", required=True)
    if let Some(sub) = parser.args.last_mut() {
        sub.required = true;
    }
    parser
}
