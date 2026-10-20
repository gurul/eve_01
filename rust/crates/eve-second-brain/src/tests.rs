//! Port of bridge/tests/test_second_brain.py, plus the second_brain half of bridge/tests/test_link_notes.py (the
//! owner's own edits: list, write, create, trash, restore, move). Each test gets its own vault in a temp dir.

use super::*;
use chrono::NaiveDate;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, SystemTime};

const DECK_PACK: &str = r#"# a pack in the deck's shape
name: daily-plan
description: What today needs
budget_tokens: 5000
include:
  - 00-vision/README.md
  - 02-todos/master.md
  - path: 08-journals/daily/*.md
    select: modified_desc
    count: 3
  - path: "03-projects/**/*.md"
    select: filename_desc
    count: 5
exclude:
  - 09-archive/**
  - "**/drafts/**"
rules:
  prefer_files:
    - 02-todos/master.md
  max_per_glob: 7
"#;

fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, min, 0).unwrap()
}

fn now() -> Option<NaiveDateTime> {
    Some(at(2026, 9, 21, 18, 30))
}

/// tmp_path: canonical, so it equals what the module resolves (macOS puts temp dirs behind /var → /private/var).
fn tmp() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = fs::canonicalize(dir.path()).unwrap();
    (dir, path)
}

/// The `vault` fixture.
fn vault() -> (tempfile::TempDir, PathBuf) {
    let (dir, base) = tmp();
    let root = base.join("vault");
    ensure_vault(&root).unwrap();
    (dir, root)
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

fn write(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
}

fn cap(root: &Path, text: &str, kind: &str, when: Option<NaiveDateTime>) -> CaptureResult {
    capture(root, text, kind, "telegram", when).unwrap()
}

fn d(root: &Path, name: &str, args: Value) -> Value {
    dispatch(root, name, &args)
}

fn ok(v: &Value) -> bool {
    v["ok"] == json!(true)
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

fn rn(root: &Path, path: &str) -> Value {
    read_note(root, path, MAX_NOTE_CHARS).unwrap()
}

fn rglob(dir: &Path, suffix: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if e.file_type().unwrap().is_dir() {
                stack.push(p.clone());
            }
            if p.to_string_lossy().ends_with(suffix) {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn today() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

// ---- skeleton ------------------------------------------------------------------------------------

#[test]
fn test_ensure_vault_creates_skeleton_and_is_idempotent() {
    let (_t, base) = tmp();
    let root = base.join("vault");
    let created = ensure_vault(&root).unwrap();
    for (name, _) in PARA {
        assert!(root.join(name).is_dir());
    }
    for f in ["CLAUDE.md", "02-todos/master.md", "00-vision/README.md", ".obsidian/app.json"] {
        assert!(created.contains(&f.to_string()), "{f}");
    }
    for wf in WORKFLOWS {
        assert!(created.contains(&format!("06-processes/{wf}.md")));
        assert!(created.contains(&format!("06-processes/packs/{wf}.pack")));
    }
    let guide = read(&root.join("CLAUDE.md"));
    for (name, _) in PARA {
        assert!(guide.contains(&format!("`{name}/`")));
    }
    let master = root.join("02-todos").join("master.md");
    assert!(["P0", "P1", "P2", "P3"].iter().all(|p| read(&master).contains(&format!("## {p}"))));
    let vision = read(&root.join("00-vision").join("README.md"));
    for heading in ["Mission", "Identity", "Principles", "10-year vision", "This year", "This quarter"] {
        assert!(vision.contains(&format!("## {heading}")));
    }
    assert!(root.join("08-journals").join("daily").is_dir());
    // idempotent: an edited file survives and nothing is reported created
    write(&master, &(read(&master) + "- [ ] keep me (added 2026-09-21)\n"));
    assert_eq!(ensure_vault(&root).unwrap(), Vec::<String>::new());
    assert!(read(&master).contains("keep me"));
}

#[test]
fn test_ensure_vault_inside_existing_obsidian_vault_adds_no_second_obsidian() {
    let (_t, base) = tmp();
    fs::create_dir(base.join(".obsidian")).unwrap();
    let root = base.join("Second Brain");
    let created = ensure_vault(&root).unwrap();
    assert!(!created.contains(&".obsidian/app.json".to_string()));
    assert!(!root.join(".obsidian").exists());
}

#[test]
fn test_default_is_off_and_configured_reads_env() {
    let (_t, base) = tmp();
    const { assert!(!SECOND_BRAIN_DEFAULT) };
    let empty = HashMap::new();
    assert!(!configured(Some(&empty)).enabled);
    assert_eq!(configured(Some(&empty)).root, *DEFAULT_VAULT);
    let env = HashMap::from([
        ("EVE_SECOND_BRAIN".to_string(), "1".to_string()),
        ("EVE_VAULT".to_string(), base.join("v").to_string_lossy().into_owned()),
    ]);
    let cfg = configured(Some(&env));
    assert!(cfg.enabled && cfg.root == base.join("v"));
    let on = HashMap::from([("EVE_SECOND_BRAIN".to_string(), "on".to_string())]);
    assert!(configured(Some(&on)).enabled);
    let off = HashMap::from([("EVE_SECOND_BRAIN".to_string(), "0".to_string())]);
    assert!(!configured(Some(&off)).enabled);
}

#[test]
fn test_configured_expands_the_home_folder() {
    let env = HashMap::from([("EVE_VAULT".to_string(), "~/notes/".to_string())]);
    assert_eq!(configured(Some(&env)).root, home().join("notes"));
}

// ---- classify and capture ------------------------------------------------------------------------

#[test]
fn test_classify_capture_rules() {
    let cases = [
        ("todo: buy milk", "todo", "buy milk"),
        ("Task: call the dentist", "todo", "call the dentist"),
        ("- [ ] fix the bike", "todo", "fix the bike"),
        ("P0 renew passport", "todo", "P0 renew passport"),
        ("p3 someday learn the cello", "todo", "p3 someday learn the cello"),
        ("journal: felt sharp this morning", "journal", "felt sharp this morning"),
        ("Dear diary, long day", "journal", "long day"),
        ("Today I finally shipped the relay", "journal", "Today I finally shipped the relay"),
        ("idea: a robot that waters plants", "idea", "a robot that waters plants"),
        ("note: the pasta place is on Castro", "note", "the pasta place is on Castro"),
        ("the pasta place is on Castro", "note", "the pasta place is on Castro"),
    ];
    for (text, kind, cleaned) in cases {
        assert_eq!(classify_capture(text), (kind, cleaned.to_string()), "{text}");
    }
}

#[test]
fn test_python_whitespace_in_patterns_includes_the_information_separators() {
    // Python's \s is str.isspace(), which takes U+001C..U+001F; the regex crate's \s does not
    assert_eq!(title_of("# \u{1c}x", "f"), "x");
    assert_eq!(classify_capture("p1\u{1c}x"), ("todo", "p1\u{1c}x".to_string()));
    assert_eq!(priority_of("p1\u{1f}x"), ("P1".to_string(), "x".to_string()));
}

#[test]
fn test_slugify() {
    assert_eq!(slugify("The Pasta Place on Castro Street is great, truly"), "the-pasta-place-on-castro-street");
    assert_eq!(slugify("Café — naïve résumé!"), "cafe-naive-resume");
    assert!(slugify(&"a".repeat(100)).len() <= 48);
    assert_eq!(slugify("???"), "note");
}

#[test]
fn test_capture_note_writes_inbox_file_with_frontmatter() {
    let (_t, vault) = vault();
    let res = cap(&vault, "The pasta place on Castro is called Doppio", "note", now());
    assert_eq!(res.kind, "note");
    assert_eq!(res.path, "01-inbox/2026-09-21-1830-the-pasta-place-on-castro-is.md");
    let text = read(&vault.join(&res.path));
    assert!(text.starts_with("---\ncreated: 2026-09-21T18:30\nsource: telegram\nkind: note\nstatus: inbox\n---\n"));
    assert!(text.trim_end().ends_with("The pasta place on Castro is called Doppio"));
    assert!(res.title.starts_with("The pasta place"));
    // a second capture with the same slug at the same minute does not overwrite
    let res2 = cap(&vault, "The pasta place on Castro is called Doppio", "note", now());
    assert_eq!(res2.path, "01-inbox/2026-09-21-1830-the-pasta-place-on-castro-is-2.md");
    assert!(vault.join(&res.path).exists() && vault.join(&res2.path).exists());
}

#[test]
fn test_capture_idea_is_an_inbox_note_of_kind_idea() {
    let (_t, vault) = vault();
    let res = cap(&vault, "a robot that waters plants", "idea", now());
    assert!(res.path.starts_with("01-inbox/") && read(&vault.join(&res.path)).contains("kind: idea"));
}

#[test]
fn test_capture_todo_priorities() {
    let (_t, vault) = vault();
    for t in ["buy milk", "p0 renew passport", "P1 book the dentist", "p3 learn the cello", "walk more"] {
        cap(&vault, t, "todo", now());
    }
    let text = read(&vault.join("02-todos").join("master.md"));
    assert!(text.contains("- [ ] renew passport (added 2026-09-21)"));
    assert!(!text.to_lowercase().contains("p0 renew"));
    let open = todos(&vault).unwrap();
    let expected: IndexMap<String, Vec<String>> = IndexMap::from([
        ("P0".to_string(), vec!["renew passport (added 2026-09-21)".to_string()]),
        ("P1".to_string(), vec!["book the dentist (added 2026-09-21)".to_string()]),
        ("P2".to_string(), vec!["buy milk (added 2026-09-21)".to_string(), "walk more (added 2026-09-21)".to_string()]),
        ("P3".to_string(), vec!["learn the cello (added 2026-09-21)".to_string()]),
    ]);
    assert_eq!(open, expected);
    // sections stay separated by a blank line so the file stays readable
    assert!(text.contains("\n\n## P1") && text.contains("\n\n## P2"));
}

#[test]
fn test_capture_journal_appends_to_todays_page() {
    let (_t, vault) = vault();
    let res = cap(&vault, "felt sharp this morning", "journal", now());
    assert_eq!(res.path, "08-journals/daily/2026-09-21.md");
    cap(&vault, "then the afternoon dragged", "journal", Some(at(2026, 9, 21, 16, 5)));
    assert_eq!(
        read(&vault.join(&res.path)),
        "# 2026-09-21\n\n- 18:30 felt sharp this morning\n- 16:05 then the afternoon dragged\n"
    );
}

#[test]
fn test_capture_rejects_empty_and_unknown_kind() {
    let (_t, vault) = vault();
    assert!(matches!(capture(&vault, "   ", "note", "telegram", None), Err(BrainError::Value(_))));
    assert_eq!(read_note(&vault, "01-inbox/a\0b.md", 10).unwrap(), json!({"ok": false, "reason": "lstat: embedded null character in path"}));
    let err = capture(&vault, "x", "poem", "telegram", None).unwrap_err();
    assert_eq!(err, BrainError::Value("unknown kind 'poem'".to_string()));
}

// ---- reading -------------------------------------------------------------------------------------

#[test]
fn test_search_scores_filename_over_body_and_skips_archive() {
    let (_t, vault) = vault();
    write(&vault.join("05-resources/pasta.md"), "# Pasta\nA list of places.\n");
    write(&vault.join("05-resources/dinner.md"), "# Dinner\nWe had pasta at Doppio, pasta was great.\n");
    fs::create_dir_all(vault.join("09-archive/05-resources")).unwrap();
    write(&vault.join("09-archive/05-resources/old-pasta.md"), "pasta pasta pasta pasta\n");
    let hits = search(&vault, "pasta", MAX_HITS, false).unwrap();
    let paths: Vec<&str> = hits.iter().map(|h| h.path.as_str()).collect();
    assert_eq!(&paths[..2], ["05-resources/pasta.md", "05-resources/dinner.md"]);
    assert!(hits[0].score == 3 + 1 && hits[1].score == 1);
    assert_eq!(hits[1].snippet, "We had pasta at Doppio, pasta was great.");
    assert!(hits.iter().all(|h| !h.path.starts_with("09-archive")));
    let archived = search(&vault, "pasta", MAX_HITS, true).unwrap();
    assert!(archived.iter().any(|h| h.path == "09-archive/05-resources/old-pasta.md"));
    assert!(search(&vault, "", MAX_HITS, false).unwrap().is_empty());
    assert_eq!(search(&vault, "pasta", 1, false).unwrap().len(), 1);
}

#[test]
fn test_search_snippet_is_clipped() {
    let (_t, vault) = vault();
    write(&vault.join("05-resources/long.md"), &("word ".repeat(200) + "\n"));
    let hit = &search(&vault, "word", MAX_HITS, false).unwrap()[0];
    assert!(hit.snippet.chars().count() <= 200);
}

#[test]
fn test_read_note_refuses_escapes_and_obsidian() {
    let (_t, vault) = vault();
    write(&vault.parent().unwrap().join("secret.md"), "not yours\n");
    for p in ["../secret.md", ".obsidian/app.json", "/etc/hosts", ""] {
        assert_eq!(rn(&vault, p)["ok"], json!(false), "{p}");
    }
    assert_eq!(rn(&vault, "05-resources/missing.md"), json!({"ok": false, "reason": "no such note"}));
    let res = rn(&vault, "02-todos/master.md");
    assert!(ok(&res) && res["path"] == "02-todos/master.md" && s(&res["text"]).contains("## P0"));
    write(&vault.join("05-resources/big.md"), &"x".repeat(100));
    let clipped = read_note(&vault, "05-resources/big.md", 10).unwrap();
    assert!(clipped["clipped"] == json!(true) && s(&clipped["text"]).len() == 10 && clipped["chars"] == 100);
}

#[test]
fn test_inbox_oldest_first() {
    let (_t, vault) = vault();
    cap(&vault, "second", "note", Some(at(2026, 9, 21, 12, 0)));
    cap(&vault, "first", "note", Some(at(2026, 9, 20, 9, 0)));
    let items = inbox(&vault).unwrap();
    assert_eq!(items.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(), ["first", "second"]);
    assert_eq!(items[0].created, "2026-09-20T09:00");
}

#[test]
fn test_active_projects_excludes_done_and_archived() {
    let (_t, vault) = vault();
    let p = vault.join("03-projects");
    fs::create_dir(p.join("eve")).unwrap();
    write(&p.join("eve/README.md"), "---\nstatus: active\n---\n# eve\n");
    fs::create_dir(p.join("garden")).unwrap();
    write(&p.join("garden/README.md"), "---\nstatus: done\n---\n");
    write(&p.join("thesis.md"), "---\nstatus: archived\n---\n");
    write(&p.join("move-house.md"), "# Move house\n");
    write(&p.join("notes.txt"), "not a note");
    assert_eq!(active_projects(&vault).unwrap(), ["eve", "move-house"]);
}

// ---- filing --------------------------------------------------------------------------------------

#[test]
fn test_file_note_moves_updates_status_and_handles_clashes() {
    let (_t, vault) = vault();
    let res = cap(&vault, "a thought about eve", "note", now());
    let new = file_note(&vault, &res.path, "03-projects/eve").unwrap();
    assert_eq!(new, "03-projects/eve/2026-09-21-1830-a-thought-about-eve.md");
    assert!(!vault.join(&res.path).exists());
    let text = read(&vault.join(&new));
    assert!(text.contains("status: filed") && !text.contains("status: inbox"));
    assert!(text.trim_end().ends_with("a thought about eve"));
    // clash: same name already there → -2
    let again = cap(&vault, "a thought about eve", "note", now());
    let new2 = file_note(&vault, &again.path, "03-projects/eve").unwrap();
    assert_eq!(new2, "03-projects/eve/2026-09-21-1830-a-thought-about-eve-2.md");
}

#[test]
fn test_a_symlinked_project_folder_is_not_a_way_out_of_the_vault() {
    // _destination did not resolve 03-projects/<x>: through a symlink, create/move/file wrote outside the vault,
    // then _rel raised and the caller heard "failed" after the file had already gone
    let (_t, vault) = vault();
    let outside = vault.parent().unwrap().join("outside");
    fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, vault.join(PROJECTS_DIR).join("away")).unwrap();
    assert!(create_note(&vault, &format!("{PROJECTS_DIR}/away"), "Escape", "").is_err());
    let note = vault.join(INBOX_DIR).join("n.md");
    write(&note, "# N\n");
    assert!(move_note(&vault, &format!("{INBOX_DIR}/n.md"), &format!("{PROJECTS_DIR}/away")).is_err());
    assert!(file_note(&vault, &format!("{INBOX_DIR}/n.md"), &format!("{PROJECTS_DIR}/away")).is_err());
    assert!(fs::read_dir(&outside).unwrap().next().is_none() && note.is_file());
    let made = create_note(&vault, &format!("{PROJECTS_DIR}/home"), "Stays", "").unwrap(); // positive control
    assert_eq!(made["path"], format!("{PROJECTS_DIR}/home/stays.md"));
}

#[test]
fn test_file_note_rejects_unknown_folder_and_escapes() {
    let (_t, vault) = vault();
    let res = cap(&vault, "x", "note", now());
    for bad in ["10-nope", "03-projects/../..", "05-resources/sub/deeper", "", "../elsewhere", "03-projects/.hidden"] {
        assert!(matches!(file_note(&vault, &res.path, bad), Err(BrainError::Value(_))), "{bad}");
    }
    assert_eq!(
        file_note(&vault, &res.path, "10-nope").unwrap_err().to_string(),
        "not a vault folder: '10-nope'"
    );
    assert!(file_note(&vault, "../outside.md", "05-resources").is_err());
    assert!(file_note(&vault, "01-inbox/missing.md", "05-resources").is_err());
    assert!(vault.join(&res.path).exists());
}

// ---- context packs -------------------------------------------------------------------------------

#[test]
fn test_parse_pack_on_the_decks_example() {
    let pack = parse_pack(DECK_PACK).unwrap();
    assert_eq!(pack.name, "daily-plan");
    assert_eq!(pack.description, "What today needs");
    assert_eq!(pack.budget_tokens, 5000);
    assert_eq!(
        pack.include,
        vec![
            Include::new("00-vision/README.md"),
            Include::new("02-todos/master.md"),
            Include::with("08-journals/daily/*.md", "modified_desc", Some(3)),
            Include::with("03-projects/**/*.md", "filename_desc", Some(5)),
        ]
    );
    assert_eq!(pack.exclude, ["09-archive/**", "**/drafts/**"]);
    assert_eq!(pack.prefer_files, ["02-todos/master.md"]);
    assert_eq!(pack.max_per_glob, 7);
}

#[test]
fn test_parse_pack_rejects_nonsense() {
    assert!(matches!(parse_pack("description: no name\n"), Err(BrainError::Value(_))));
    let err = parse_pack("name: x\ninclude:\n  - path: a/*.md\n    select: random\n").unwrap_err();
    assert_eq!(err, BrainError::Value("unknown select 'random'".to_string()));
    assert!(matches!(parse_pack(""), Err(BrainError::Value(_))));
}

#[test]
fn test_parse_pack_follows_pythons_types() {
    // a bad count is int()'s ValueError; a scalar where a list belongs is iterated as Python iterates it
    let bad = parse_pack("name: x\ninclude:\n  - path: a.md\n    count: lots\n").unwrap_err();
    assert_eq!(bad, BrainError::Value("invalid literal for int() with base 10: 'lots'".to_string()));
    let typed = parse_pack("name: x\nbudget_tokens: 12\nexclude: ab\nrules:\n  max_per_glob: 3 # inline\n").unwrap();
    assert_eq!(typed.exclude, ["a", "b"]);
    assert_eq!((typed.budget_tokens, typed.max_per_glob), (12, 3));
    let not_iterable = parse_pack("name: x\ninclude: 5\n").unwrap_err();
    assert_eq!(not_iterable.type_name(), "TypeError");
    assert_eq!(parse_pack("name: 7\n").unwrap().name, "7");
    assert_eq!(parse_pack("name: x\n").unwrap().budget_tokens, 4000);
    // PackDef's fields are converted in order, so a bad budget is reported before a bad prefer_files
    let first = parse_pack("name: x\nbudget_tokens: abc\nrules:\n  prefer_files: 7\n").unwrap_err();
    assert_eq!(first, BrainError::Value("invalid literal for int() with base 10: 'abc'".to_string()));
    // Python's \d and int() take every Unicode decimal digit, and its ints are unbounded
    let arabic = parse_pack("name: \u{663}\nbudget_tokens: \u{661}\u{662}\n").unwrap();
    assert_eq!((arabic.name.as_str(), arabic.budget_tokens), ("3", 12));
    assert_eq!(parse_pack("name: x\nbudget_tokens: ' \u{967}_0 '\n").unwrap().budget_tokens, 10);
    let big = parse_pack("name: 0099999999999999999999\nbudget_tokens: 99999999999999999999\n").unwrap();
    assert_eq!((big.name.as_str(), big.budget_tokens), ("99999999999999999999", i64::MAX));
    assert_eq!(parse_pack("name: x\nmax_per_glob: -99999999999999999999\n").unwrap().max_per_glob, i64::MIN);
}

#[test]
fn test_str_of_a_json_float_is_pythons() {
    use crate::py::float_repr;
    for (f, want) in [
        (1.5, "1.5"),
        (1.0, "1.0"),
        (-0.0, "-0.0"),
        (1e20, "1e+20"),
        (1e16, "1e+16"),
        (123456789012345.6, "123456789012345.6"),
        (0.0001, "0.0001"),
        (0.00001, "1e-05"),
        (1.25e-7, "1.25e-07"),
        (0.1 + 0.2, "0.30000000000000004"),
    ] {
        assert_eq!(float_repr(f), want, "{f}");
    }
    assert_eq!(value_str(&json!([1.5, 1e20, "a"])), "[1.5, 1e+20, 'a']");
}

#[test]
fn test_builtin_packs_parse_and_load() {
    let (_t, vault) = vault();
    for wf in WORKFLOWS {
        let pack = load_pack(&vault, wf).unwrap();
        assert!(pack.name == wf && !pack.include.is_empty() && pack.budget_tokens > 0);
    }
    assert!(load_pack(&vault, "nope").is_err());
    assert!(load_pack(&vault, "../CLAUDE").is_err());
}

#[test]
fn test_compile_pack_orders_selects_and_excludes() {
    let (_t, vault) = vault();
    let daily = vault.join("08-journals/daily");
    for day in ["2026-09-18", "2026-09-19", "2026-09-20", "2026-09-21"] {
        write(&daily.join(format!("{day}.md")), &format!("# {day}\n- 09:00 line\n"));
    }
    fs::create_dir_all(vault.join("03-projects/eve/drafts")).unwrap();
    write(&vault.join("03-projects/eve/README.md"), "# eve\n");
    write(&vault.join("03-projects/eve/drafts/wip.md"), "# wip\n");
    fs::create_dir_all(vault.join("09-archive/03-projects")).unwrap();
    write(&vault.join("09-archive/03-projects/gone.md"), "# gone\n");
    let pack = parse_pack(&DECK_PACK.replace("select: modified_desc", "select: filename_desc")).unwrap();
    let out = compile_pack(&vault, &pack, now()).unwrap();
    assert_eq!(out.files[0], "02-todos/master.md"); // preferred first
    assert_eq!(out.files[1], "00-vision/README.md");
    let journals: Vec<&String> = out.files.iter().filter(|f| f.starts_with("08-journals")).collect();
    assert_eq!(
        journals,
        ["08-journals/daily/2026-09-21.md", "08-journals/daily/2026-09-20.md", "08-journals/daily/2026-09-19.md"]
    ); // newest three by name
    assert!(out.files.contains(&"03-projects/eve/README.md".to_string()));
    assert!(out.files.iter().all(|f| !f.contains("drafts") && !f.starts_with("09-archive")));
    assert!(out.dropped.is_empty());
    assert!(out.xml.starts_with("<context pack=\"daily-plan\" compiled=\"2026-09-21\">\n<file path=\"02-todos/master.md\">\n"));
    assert!(out.xml.trim_end().ends_with("</file>\n</context>"));
    assert_eq!(out.xml.matches("<file ").count(), out.files.len());
    assert_eq!(out.tokens, out.xml.chars().count() / 4);
}

#[test]
fn test_compile_pack_respects_budget_and_reports_drops() {
    let (_t, vault) = vault();
    write(&vault.join("05-resources/a-big.md"), &("A".repeat(4000) + "\n"));
    write(&vault.join("05-resources/b-big.md"), &("B".repeat(4000) + "\n"));
    write(&vault.join("05-resources/c-small.md"), "small\n");
    let mut pack = PackDef::new("t", "", 1200);
    pack.include = vec![Include::with("05-resources/*.md", "filename_desc", None)];
    pack.prefer_files = vec!["05-resources/a-big.md".to_string()];
    let out = compile_pack(&vault, &pack, now()).unwrap();
    // filename_desc gives c, b, a; preference moves a to the front; the budget (~1200 tokens = 4800 chars)
    // fits one big file, so the tail — b — is dropped and c (small) survives ahead of it.
    assert_eq!(out.files, ["05-resources/a-big.md", "05-resources/c-small.md"]);
    assert_eq!(out.dropped, ["05-resources/b-big.md"]);
    assert!(out.tokens <= 1200);
    assert!(!out.xml.contains("BBBB") && out.xml.contains("small"));
    let mut tiny_pack = PackDef::new("t", "", 10);
    tiny_pack.include = pack.include.clone();
    let tiny = compile_pack(&vault, &tiny_pack, None).unwrap();
    assert!(tiny.files.is_empty() && tiny.dropped.len() == 3 && tiny.xml.ends_with("</context>"));
}

#[test]
fn test_compile_pack_modified_desc_and_count_and_max_per_glob() {
    let (_t, vault) = vault();
    let folder = vault.join("05-resources");
    let base = SystemTime::now() - Duration::from_secs(1000);
    for (i, name) in ["old", "mid", "new"].iter().enumerate() {
        let p = folder.join(format!("{name}.md"));
        write(&p, &format!("# {name}\n"));
        let f = fs::File::options().write(true).open(&p).unwrap();
        f.set_modified(base + Duration::from_secs(i as u64 * 100)).unwrap();
    }
    let mut pack = PackDef::new("t", "", 9999);
    pack.include = vec![Include::with("05-resources/*.md", "modified_desc", Some(2))];
    assert_eq!(compile_pack(&vault, &pack, None).unwrap().files, ["05-resources/new.md", "05-resources/mid.md"]);
    let mut capped = PackDef::new("t", "", 9999);
    capped.include = vec![Include::with("05-resources/*.md", "modified_desc", Some(5))];
    capped.max_per_glob = 1;
    assert_eq!(compile_pack(&vault, &capped, None).unwrap().files, ["05-resources/new.md"]);
}

#[test]
fn test_fnmatch_and_glob_follow_python() {
    assert!(py::fnmatch("09-archive/a/b.md", "09-archive/**"));
    assert!(py::fnmatch("03-projects/eve/drafts/wip.md", "**/drafts/**"));
    assert!(!py::fnmatch("03-projects/eve/README.md", "**/drafts/**"));
    assert!(py::fnmatch("a.md", "[!b].md") && !py::fnmatch("b.md", "[!b].md"));
    assert!(py::fnmatch("c.md", "[a-d].md") && !py::fnmatch("e.md", "[a-d].md"));
    assert!(py::fnmatch("[x", "[x") && py::fnmatch("a.b", "a?b"));
    assert!(!py::fnmatch("A.md", "a.md"));
    let (_t, vault) = vault();
    fs::create_dir_all(vault.join("03-projects/x/y")).unwrap();
    write(&vault.join("03-projects/top.md"), "t");
    write(&vault.join("03-projects/x/y/deep.md"), "d");
    let mut found: Vec<String> = py::glob(&vault, "03-projects/**/*.md")
        .unwrap()
        .iter()
        .map(|p| py::posix(p.strip_prefix(&vault).unwrap()))
        .collect();
    found.sort();
    assert_eq!(found, ["03-projects/top.md", "03-projects/x/y/deep.md"]);
}

// ---- workflows -----------------------------------------------------------------------------------

#[test]
fn test_workflow_prompt_for_every_name_has_sop_and_context() {
    let (_t, vault) = vault();
    cap(&vault, "buy milk", "todo", now());
    cap(&vault, "felt sharp", "journal", now());
    cap(&vault, "an inbox thought", "note", now());
    fs::create_dir(vault.join("03-projects/eve")).unwrap();
    write(&vault.join("03-projects/eve/README.md"), "# eve\nthe desk robot\n");
    for wf in WORKFLOWS {
        let sop = read(&vault.join(format!("06-processes/{wf}.md")));
        let prompt = workflow_prompt(&vault, wf, "", now()).unwrap();
        assert!(prompt.starts_with(sop.trim_end()));
        assert!(prompt.contains("Today is Monday 2026-09-21."));
        assert!(prompt.contains("Active projects: eve."));
        assert!(prompt.contains(&format!("<context pack=\"{wf}\"")) && prompt.contains("</context>"));
        if wf != "distill-chat" {
            assert!(prompt.trim_end().ends_with("</context>"));
        }
    }
    let daily = workflow_prompt(&vault, "daily-plan", "", now()).unwrap();
    assert!(daily.contains("<file path=\"02-todos/master.md\">") && daily.contains("buy milk"));
    assert!(daily.contains("<file path=\"08-journals/daily/2026-09-21.md\">") && daily.contains("felt sharp"));
    let triage = workflow_prompt(&vault, "triage-inbox", "", now()).unwrap();
    assert!(triage.contains("an inbox thought") && triage.contains("<file path=\"03-projects/eve/README.md\">"));
    let distilled = workflow_prompt(&vault, "distill-chat", "A: let's ship it\nB: agreed", now()).unwrap();
    assert!(distilled.trim_end().ends_with("<transcript>\nA: let's ship it\nB: agreed\n</transcript>"));
    let empty = workflow_prompt(&vault, "distill-chat", "", now()).unwrap();
    assert!(empty.contains("no transcript was supplied"));
    assert!(matches!(workflow_prompt(&vault, "make-coffee", "", None), Err(BrainError::Value(_))));
}

// ---- the tools -----------------------------------------------------------------------------------

#[test]
fn test_every_tool_is_strict_named_and_dispatched() {
    let (_t, vault) = vault();
    assert_eq!(SECOND_BRAIN_TOOLS.len(), 9);
    for tool in SECOND_BRAIN_TOOLS.iter() {
        assert!(tool["type"] == "function" && tool["strict"] == json!(true));
        let params = &tool["parameters"];
        assert!(params["type"] == "object" && params["additionalProperties"] == json!(false));
        let mut required: Vec<&str> = params["required"].as_array().unwrap().iter().map(s).collect();
        let mut props: Vec<&str> = params["properties"].as_object().unwrap().keys().map(String::as_str).collect();
        required.sort();
        props.sort();
        assert_eq!(required, props); // strict mode: every property required
        let name = s(&tool["name"]);
        assert!(SECOND_BRAIN_TOOL_NAMES.contains(&name));
        let res = d(&vault, name, json!({}));
        assert!(res.is_object() && res.get("ok").is_some());
        serde_json::to_string(&res).unwrap();
    }
    let mut names: Vec<&str> = SECOND_BRAIN_TOOL_NAMES.to_vec();
    names.sort();
    let mut expected = vec![
        "capture_note", "search_notes", "read_note", "list_inbox", "file_note", "list_todos",
        "second_brain_workflow", "edit_note", "undo_note",
    ];
    expected.sort();
    assert_eq!(names, expected);
    let schema_order: Vec<&str> = SECOND_BRAIN_TOOLS.iter().map(|t| s(&t["name"])).collect();
    assert_eq!(schema_order, SECOND_BRAIN_TOOL_NAMES); // the tuple follows the schema list
}

#[test]
fn test_dispatch_never_raises_and_returns_ok_false_for_bad_input() {
    let (_t, vault) = vault();
    assert_eq!(d(&vault, "capture_note", json!({"text": ""}))["ok"], json!(false));
    assert_eq!(
        d(&vault, "capture_note", json!({"text": "x", "kind": "poem"})),
        json!({"ok": false, "reason": "kind must be one of note, todo, journal, idea"})
    );
    assert_eq!(d(&vault, "read_note", json!({"path": "../x.md"}))["ok"], json!(false));
    assert_eq!(d(&vault, "read_note", json!({"path": ".obsidian/app.json"}))["ok"], json!(false));
    assert_eq!(d(&vault, "file_note", json!({"path": "01-inbox/nope.md", "into": "05-resources"}))["ok"], json!(false));
    assert_eq!(
        d(&vault, "second_brain_workflow", json!({"name": "nope", "extra": null})),
        json!({"ok": false, "reason": "unknown workflow 'nope'"})
    );
    assert_eq!(d(&vault, "no_such_tool", json!({})), json!({"ok": false, "reason": "unknown tool no_such_tool"}));
    assert_eq!(d(&vault, "search_notes", Value::Null)["ok"], json!(true));
    let missing = vault.join("not-there");
    assert_eq!(d(&missing, "list_inbox", json!({})), json!({"ok": true, "items": [], "count": 0}));
    assert_eq!(d(&missing, "list_todos", json!({}))["todos"], json!({"P0": [], "P1": [], "P2": [], "P3": []}));
    assert!(!missing.exists()); // reading never creates a vault; only capture and the workflows do
}

#[test]
fn test_dispatch_round_trip() {
    let (_t, vault) = vault();
    let saved = d(&vault, "capture_note", json!({"text": "idea: a robot that waters plants", "kind": null}));
    assert!(ok(&saved) && saved["kind"] == "idea" && s(&saved["path"]).starts_with("01-inbox/"));
    assert_eq!(s(&saved["line"]), format!("Saved to {}", s(&saved["path"])));
    let todo = d(&vault, "capture_note", json!({"text": "todo: p1 water the plants", "kind": null}));
    assert!(ok(&todo) && todo["kind"] == "todo" && s(&todo["line"]).contains("todo list"));
    assert_eq!(
        d(&vault, "list_todos", json!({}))["todos"]["P1"],
        json!([format!("water the plants (added {})", today())])
    );
    let forced = d(&vault, "capture_note", json!({"text": "this reads like a note but is a journal line", "kind": "journal"}));
    assert!(forced["kind"] == "journal" && s(&forced["path"]).starts_with("08-journals/daily/"));
    let found = d(&vault, "search_notes", json!({"query": "waters plants"}));
    assert!(ok(&found) && found["hits"][0]["path"] == saved["path"]);
    let keys: Vec<&String> = found.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["ok", "hits", "count"]);
    let note = d(&vault, "read_note", json!({"path": saved["path"]}));
    assert!(ok(&note) && s(&note["text"]).contains("waters plants"));
    let listed = d(&vault, "list_inbox", json!({}));
    assert!(listed["count"] == 1 && listed["items"][0]["path"] == saved["path"]);
    let moved = d(&vault, "file_note", json!({"path": saved["path"], "into": "04-areas/garden"}));
    assert!(ok(&moved) && s(&moved["path"]).starts_with("04-areas/garden/"));
    let wf = d(&vault, "second_brain_workflow", json!({"name": "daily-plan", "extra": null}));
    assert!(ok(&wf) && s(&wf["note"]).contains("calendar"));
    let review = d(&vault, "second_brain_workflow", json!({"name": "weekly-review", "extra": null}));
    assert!(ok(&review) && review["note"] == "hand this prompt to think_hard");
    assert!(s(&wf["prompt"]).contains("water the plants"));
    assert_eq!(wf["tokens"], json!(s(&wf["prompt"]).chars().count() / 4));
}

#[test]
fn test_dispatch_clips_long_notes() {
    let (_t, vault) = vault();
    write(&vault.join("05-resources/huge.md"), &"z".repeat(10000));
    let res = d(&vault, "read_note", json!({"path": "05-resources/huge.md"}));
    assert!(ok(&res) && res["clipped"] == json!(true) && s(&res["text"]).chars().count() < 5000);
    assert!(s(&res["text"]).ends_with(" …"));
}

#[test]
fn test_instructions_block_mentions_the_tools_and_workflows() {
    for name in ["capture_note", "search_notes", "read_note", "list_todos", "second_brain_workflow", "think_hard",
                 "edit_note", "undo_note"] {
        assert!(INSTRUCTIONS_BLOCK.contains(name));
    }
    for phrase in ["plan my day", "weekly review", "triage my inbox", "distill this"] {
        assert!(INSTRUCTIONS_BLOCK.contains(phrase));
    }
}

fn edit(root: &Path, path: &str, old: &str, new: &str) -> Value {
    let note = d(root, "read_note", json!({"path": path}));
    assert!(ok(&note));
    d(root, "edit_note", json!({"path": path, "revision": note["revision"], "old_text": old, "new_text": new}))
}

#[test]
fn test_edit_shopping_list_keeps_one_file_and_exact_frontmatter() {
    let (_t, vault) = vault();
    let path = cap(&vault, "# Shopping\n- [ ] wipes\n- [ ] bathroom mat", "note", now()).path;
    let file = vault.join(&path);
    let original = read(&file).replace("kind: note", "kind: note\ntags:\n  - home\ncustom: 'keep this'");
    write(&file, &original);
    let before_paths = rglob(&vault, ".md");
    let result = edit(&vault, &path, "", "- [ ] alcohol wipes for electronics");
    assert!(ok(&result) && result["path"] == path.as_str() && result["changed"] == json!(true));
    assert_eq!(read(&file), original.clone() + "- [ ] alcohol wipes for electronics\n");
    assert_eq!(rglob(&vault, ".md"), before_paths);
    assert!(ok(&edit(&vault, &path, "- [ ] wipes", "- [x] wipes")));
    assert!(ok(&edit(&vault, &path, "- [ ] bathroom mat\n", "")));
    assert_eq!(
        read(&file),
        original.replace("- [ ] wipes", "- [x] wipes").replace("- [ ] bathroom mat\n", "")
            + "- [ ] alcohol wipes for electronics\n"
    );
    let hits = search(&vault, "electronics", MAX_HITS, false).unwrap();
    assert_eq!(hits.iter().map(|h| h.path.clone()).collect::<Vec<_>>(), [path]);
}

#[test]
fn test_todo_completion_reopen_and_priority_change() {
    let (_t, vault) = vault();
    cap(&vault, "p2 buy milk", "todo", now());
    cap(&vault, "p1 call Sam", "todo", now());
    let line = "- [ ] buy milk (added 2026-09-21)";
    let done = line.replace("[ ]", "[x]");
    assert!(ok(&edit(&vault, TODOS_FILE, line, &done)));
    assert!(todos(&vault).unwrap()["P2"].is_empty());
    assert!(ok(&edit(&vault, TODOS_FILE, &done, line)));
    assert_eq!(todos(&vault).unwrap()["P2"], ["buy milk (added 2026-09-21)"]);
    let old = read(&vault.join(TODOS_FILE));
    let new = old.replace(&format!("{line}\n"), "").replace("## P0\n", &format!("## P0\n{line}\n"));
    assert!(ok(&edit(&vault, TODOS_FILE, &old, &new)));
    let t = todos(&vault).unwrap();
    assert_eq!(t["P0"], ["buy milk (added 2026-09-21)"]);
    assert!(t["P2"].is_empty());
    assert_eq!(t["P1"], ["call Sam (added 2026-09-21)"]);
}

#[test]
fn test_edit_refuses_stale_ambiguous_missing_or_frontmatter_matches() {
    let (_t, vault) = vault();
    let path = cap(&vault, "milk\nmilk\nkeep me", "note", now()).path;
    let before = read(&vault.join(&path));
    let stale = rn(&vault, &path)["revision"].clone();
    for old in ["milk", "not present", "status: inbox"] {
        let result = edit(&vault, &path, old, "oops");
        assert!(!ok(&result) && s(&result["reason"]).contains("exactly once"));
        assert_eq!(read(&vault.join(&path)), before);
    }
    assert!(ok(&edit(&vault, &path, "milk\nmilk", "oat milk"))); // positive control: context disambiguates
    let current = read(&vault.join(&path));
    for revision in [stale, json!("")] {
        let result = d(&vault, "edit_note", json!({"path": path, "revision": revision, "old_text": "keep me", "new_text": "lost"}));
        assert!(!ok(&result) && s(&result["reason"]).contains("read_note"));
        assert_eq!(read(&vault.join(&path)), current);
    }
}

#[test]
fn test_undo_restores_bytes_and_walks_history_without_toggling() {
    let (_t, vault) = vault();
    let path = cap(&vault, "original", "note", now()).path;
    let original = fs::read(vault.join(&path)).unwrap();
    let first = edit(&vault, &path, "original", "second");
    let second = edit(&vault, &path, "second", "original"); // deliberately return to a previous state
    assert!(ok(&first) && ok(&second));
    for (expected, change) in [(&b"second\n"[..], &second), (&b"original\n"[..], &first)] {
        let note = rn(&vault, &path);
        assert_eq!(note["undo_id"], change["undo_id"]);
        let result =
            d(&vault, "undo_note", json!({"path": path, "undo_id": note["undo_id"], "revision": note["revision"]}));
        assert!(ok(&result) && fs::read(vault.join(&path)).unwrap().ends_with(expected));
    }
    assert_eq!(fs::read(vault.join(&path)).unwrap(), original);
    assert_eq!(rn(&vault, &path)["undo_id"], Value::Null);
    assert_eq!(rglob(&vault.join(HISTORY_DIR), ".undone").len(), 2);
}

#[test]
fn test_undo_refuses_later_obsidian_changes_and_wrong_note() {
    let (_t, vault) = vault();
    let path = cap(&vault, "original", "note", now()).path;
    let change = edit(&vault, &path, "original", "second");
    let other = cap(&vault, "second", "note", now()).path;
    let other_rev = rn(&vault, &other)["revision"].clone();
    assert!(!ok(&d(&vault, "undo_note", json!({"path": other, "undo_id": change["undo_id"], "revision": other_rev}))));
    write(&vault.join(&path), "a later manual edit\n");
    for revision in [change["revision"].clone(), rn(&vault, &path)["revision"].clone()] {
        let result = d(&vault, "undo_note", json!({"path": path, "undo_id": change["undo_id"], "revision": revision}));
        assert!(!ok(&result) && s(&result["reason"]).contains("note changed"));
        assert_eq!(read(&vault.join(&path)), "a later manual edit\n");
    }
    assert_eq!(rn(&vault, &path)["undo_id"], Value::Null);
}

#[test]
fn test_edit_paths_history_and_hidden_search() {
    let (_t, vault) = vault();
    let outside = vault.parent().unwrap().join("outside.md");
    write(&outside, "outside");
    std::os::unix::fs::symlink(&outside, vault.join("link.md")).unwrap();
    let hidden = vault.join(".obsidian/hidden.md");
    write(&hidden, "hidden");
    let outside_str = outside.to_string_lossy().into_owned();
    for path in ["../outside.md", outside_str.as_str(), "link.md", ".obsidian/hidden.md", "02-todos/nope.md"] {
        for name in ["edit_note", "undo_note"] {
            let args = json!({"path": path, "revision": "x", "undo_id": "../x", "old_text": "", "new_text": "oops"});
            assert!(!ok(&d(&vault, name, args)), "{name} {path}");
        }
    }
    assert!(read(&outside) == "outside" && read(&hidden) == "hidden");
    fs::remove_file(vault.join("link.md")).unwrap();
    let path = cap(&vault, "# Test\nsecretcobalt", "note", now()).path;
    let paths = |q: &str| search(&vault, q, MAX_HITS, false).unwrap().into_iter().map(|h| h.path).collect::<Vec<_>>();
    assert_eq!(paths("secretcobalt"), std::slice::from_ref(&path)); // positive control
    assert!(ok(&edit(&vault, &path, "secretcobalt", "replacement")));
    assert!(paths("secretcobalt").is_empty());
    let record = rglob(&vault.join(HISTORY_DIR), ".json").remove(0);
    assert!(read(&record).contains("secretcobalt"));
    let record_rel = py::posix(record.strip_prefix(&vault).unwrap());
    assert_eq!(rn(&vault, &record_rel)["ok"], json!(false));
    let mut pack = PackDef::new("all", "", 100000);
    pack.include = vec![Include::new("**/*")];
    assert!(!compile_pack(&vault, &pack, None).unwrap().xml.contains("secretcobalt"));
}

#[test]
fn test_history_record_is_pythons_json() {
    let (_t, vault) = vault();
    let path = cap(&vault, "a \"quoted\" line\twith tab", "note", now()).path;
    let change = edit(&vault, &path, "line", "row");
    let folder = vault.join(HISTORY_DIR).join(revision(&path));
    let record = read(&folder.join(format!("{}.json", s(&change["undo_id"]))));
    let before = read(&vault.join(&path)).replace("row", "line");
    let expected = format!(
        "{{\"path\": \"{path}\", \"before\": {}, \"after\": \"{}\"}}",
        serde_json::to_string(&before).unwrap(),
        s(&change["revision"])
    );
    assert_eq!(record, expected);
}

#[test]
fn test_history_symlink_and_failed_backup_leave_note_unchanged() {
    let (_t, vault) = vault();
    let path = cap(&vault, "keep", "note", now()).path;
    let revision = rn(&vault, &path)["revision"].clone();
    let before = fs::read(vault.join(&path)).unwrap();
    let external = vault.parent().unwrap().join("external");
    fs::create_dir(&external).unwrap();
    std::os::unix::fs::symlink(&external, vault.join(HISTORY_DIR)).unwrap();
    let args = json!({"path": path, "revision": revision, "old_text": "keep", "new_text": "lose"});
    assert!(!ok(&d(&vault, "edit_note", args)));
    assert!(fs::read(vault.join(&path)).unwrap() == before && fs::read_dir(&external).unwrap().next().is_none());
    fs::remove_file(vault.join(HISTORY_DIR)).unwrap();

    seams::set_atomic_text(|_, _| Err(BrainError::Os { kind: "OSError", message: "disk full".to_string() }));
    let result = edit(&vault, &path, "keep", "lose");
    seams::clear();
    assert!(!ok(&result) && result["reason"] == "disk full");
    assert_eq!(fs::read(vault.join(&path)).unwrap(), before);
}

#[test]
fn test_clipped_reads_support_targeted_edits_without_truncation() {
    let (_t, vault) = vault();
    let body = format!("visible passage\n{}\nkeep this tail\n", "z".repeat(10000));
    let path = cap(&vault, &body, "note", now()).path;
    let before = read(&vault.join(&path));
    assert_eq!(d(&vault, "read_note", json!({"path": path}))["clipped"], json!(true));
    assert!(ok(&edit(&vault, &path, "visible passage", "changed passage")));
    assert_eq!(read(&vault.join(&path)), before.replace("visible passage", "changed passage"));
}

#[test]
fn test_two_simultaneous_edits_cannot_overwrite_each_other() {
    let (_t, vault) = vault();
    let path = cap(&vault, "keep", "note", now()).path;
    let revision = rn(&vault, &path)["revision"].clone();
    let workers: Vec<_> = ["one", "two"]
        .into_iter()
        .map(|text| {
            let (vault, path, revision) = (vault.clone(), path.clone(), revision.clone());
            std::thread::spawn(move || {
                dispatch(&vault, "edit_note", &json!({"path": path, "revision": revision, "old_text": "", "new_text": text}))
            })
        })
        .collect();
    let results: Vec<Value> = workers.into_iter().map(|w| w.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| ok(r)).count(), 1);
    let text = read(&vault.join(&path));
    assert!(text.ends_with("keep\none\n") || text.ends_with("keep\ntwo\n"));
}

#[test]
fn test_todo_capture_and_edit_cannot_silently_lose_a_change() {
    let (_t, vault) = vault();
    cap(&vault, "first", "todo", now());
    let revision = rn(&vault, TODOS_FILE)["revision"].clone();
    let (tx, rx) = mpsc::channel::<Value>();
    let worker: std::rc::Rc<std::cell::RefCell<Option<std::thread::JoinHandle<()>>>> = Default::default();
    let slot = worker.clone();
    let (hook_vault, hook_rev) = (vault.clone(), revision.clone());
    seams::set_before_append_under(move || {
        // An edit lands between the capture's read and its write.
        let (v, r, tx) = (hook_vault.clone(), hook_rev.clone(), tx.clone());
        let handle = std::thread::spawn(move || {
            let res = dispatch(&v, "edit_note", &json!({"path": TODOS_FILE, "revision": r, "old_text": "", "new_text": "edited line"}));
            let _ = tx.send(res);
        });
        std::thread::sleep(Duration::from_millis(300));
        *slot.borrow_mut() = Some(handle);
    });
    cap(&vault, "second", "todo", now());
    seams::clear();
    let handle = worker.borrow_mut().take().expect("the hook ran");
    handle.join().unwrap();
    let edit_result = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let text = read(&vault.join(TODOS_FILE));
    assert!(text.contains("second (added 2026-09-21)"));
    // Either both changes survive, or the edit was refused as stale; never "ok" and lost.
    assert!(edit_result["ok"] == json!(false) || text.contains("edited line"));
}

#[test]
fn test_failed_note_write_keeps_original_and_does_not_offer_failed_undo() {
    let (_t, vault) = vault();
    let path = cap(&vault, "keep", "note", now()).path;
    let original = read(&vault.join(&path));
    let note_path = vault.join(&path);
    seams::set_replace(move |_, dst| (dst == note_path).then(|| std::io::Error::from_raw_os_error(5)));
    let result = edit(&vault, &path, "keep", "changed");
    seams::clear();
    assert!(!ok(&result) && read(&vault.join(&path)) == original);
    assert!(s(&result["reason"]).starts_with("[Errno 5] "));
    assert_eq!(rglob(&vault.join(HISTORY_DIR), ".failed").len(), 1);
    let leftovers: Vec<_> = fs::read_dir(vault.join(INBOX_DIR))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(".eve-"))
        .collect();
    assert!(leftovers.is_empty());
    // Even if a later manual edit matches the failed write, it must not gain an undo entry.
    write(&vault.join(&path), &original.replace("keep", "changed"));
    assert_eq!(rn(&vault, &path)["undo_id"], Value::Null);
}

#[test]
fn test_edit_rechecks_after_backup_when_an_external_editor_changes_note() {
    let (_t, vault) = vault();
    let path = cap(&vault, "keep", "note", now()).path;
    let note_path = vault.join(&path);
    seams::set_atomic_text(move |destination, text| {
        seams::real_atomic_text(destination, text)?;
        if py::suffix(destination) == ".json" {
            fs::write(&note_path, "edited in Obsidian\n").unwrap();
        }
        Ok(())
    });
    let result = edit(&vault, &path, "keep", "changed");
    seams::clear();
    assert!(!ok(&result) && s(&result["reason"]).contains("note changed"));
    assert_eq!(read(&vault.join(&path)), "edited in Obsidian\n");
}

#[test]
fn test_the_daily_plan_is_written_in_one_pass_with_the_calendar() {
    // 2026-09-24 08:44: "plan my day" went vault → think_hard and came back as "a suggested plan … with no
    // calendar checked". The brain that holds the calendar tools writes the plan itself.
    let (_t, vault) = vault();
    let wf = d(&vault, "second_brain_workflow", json!({"name": "daily-plan", "extra": null}));
    assert!(!s(&wf["note"]).contains("think_hard") && s(&wf["note"]).contains("calendar"));
    assert!(s(&wf["prompt"]).split("<context").next().unwrap().contains("calendar"));
    let tool = SECOND_BRAIN_TOOLS.iter().find(|t| t["name"] == "second_brain_workflow").unwrap();
    assert!(!s(&tool["description"]).contains("think_hard"));
    let flat = INSTRUCTIONS_BLOCK.replace('\n', " ");
    let sentence = flat.split(". ").find(|x| x.contains("plan my day")).unwrap();
    assert!(!sentence.contains("think_hard") && sentence.contains("calendar"));
}

// ---- the owner's own edits (from test_link_notes.py) -------------------------------------------------

/// test_link_notes.py's `vault` fixture.
fn notes_vault() -> (tempfile::TempDir, PathBuf) {
    let (t, root) = vault();
    let tea = root.join("01-inbox/tea.md");
    write(&tea, "---\ncreated: 2026-10-01\nsource: telegram\n---\n# Tea\n\n- [ ] buy oolong\n- [x] boil water\n");
    fs::create_dir(root.join("03-projects/garden")).unwrap();
    write(&root.join("03-projects/garden/beds.md"), "# Beds\nRaised beds by the fence.\n");
    let f = fs::File::options().write(true).open(&tea).unwrap();
    f.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000_000)).unwrap(); // the newest
    (t, root)
}

#[test]
fn test_list_is_every_note_newest_first_with_folders() {
    let (_t, root) = notes_vault();
    let rows = list_notes(&root).unwrap();
    assert_eq!(
        rows[0],
        NoteRow {
            path: "01-inbox/tea.md".into(),
            folder: "01-inbox".into(),
            title: "Tea".into(),
            preview: "buy oolong".into(),
            modified: 2_000_000_000
        }
    );
    let paths: Vec<&str> = rows.iter().map(|n| n.path.as_str()).collect();
    assert!(paths.contains(&"03-projects/garden/beds.md") && paths.contains(&"CLAUDE.md"));
    assert!(!paths.iter().any(|p| p.starts_with('.') || p.contains("/.")));
    let folders = note_folders(&root).unwrap();
    assert!(folders.contains(&"03-projects/garden".to_string()) && folders[..2] == ["00-vision", "01-inbox"]);
}

#[test]
fn test_a_captures_preview_is_not_its_title_again() {
    let (_t, root) = notes_vault();
    write(&root.join("01-inbox/cap.md"), "---\nkind: note\n---\nThe blue cafe is good\n\nGo on a Tuesday\n");
    write(&root.join("01-inbox/one.md"), "Only a line\n");
    let rows: HashMap<String, NoteRow> = list_notes(&root).unwrap().into_iter().map(|n| (n.path.clone(), n)).collect();
    assert_eq!(rows["01-inbox/cap.md"].title, "The blue cafe is good");
    assert_eq!(rows["01-inbox/cap.md"].preview, "Go on a Tuesday");
    assert_eq!(rows["01-inbox/one.md"].preview, "");
    assert_eq!(rows["03-projects/garden/beds.md"].preview, "Raised beds by the fence.");
}

#[test]
fn test_read_splits_the_frontmatter_off_and_save_keeps_it() {
    let (_t, root) = notes_vault();
    let note = rn(&root, "01-inbox/tea.md");
    let (_, body) = split_frontmatter(s(&note["text"]));
    let body = body.replace("- [ ] buy oolong", "- [x] buy oolong");
    let saved = write_note(&root, "01-inbox/tea.md", s(&note["revision"]), &body).unwrap();
    assert!(ok(&saved) && saved["changed"] == json!(true) && !s(&saved["undo_id"]).is_empty());
    let text = read(&root.join("01-inbox/tea.md"));
    assert!(text.starts_with("---\ncreated: 2026-10-01\nsource: telegram\n---\n# Tea") && text.contains("- [x] buy oolong"));
    // the same body again changes nothing
    let again = write_note(&root, "01-inbox/tea.md", s(&saved["revision"]), &body).unwrap();
    assert!(ok(&again) && again["changed"] == json!(false));
    // and undo takes the save back
    let undone = undo_note(&root, "01-inbox/tea.md", s(&saved["undo_id"]), s(&saved["revision"])).unwrap();
    assert!(ok(&undone) && read(&root.join("01-inbox/tea.md")).contains("- [ ] buy oolong"));
}

#[test]
fn test_a_stale_revision_is_refused_and_nothing_is_written() {
    let (_t, root) = notes_vault();
    let note = rn(&root, "01-inbox/tea.md");
    write(&root.join("01-inbox/tea.md"), "# Tea\nchanged in Obsidian\n");
    let stale = s(&note["revision"]);
    assert!(write_note(&root, "01-inbox/tea.md", stale, "mine").is_err());
    assert!(trash_note(&root, "01-inbox/tea.md", stale).is_err());
    assert!(write_note(&root, "01-inbox/tea.md", "", "mine").is_err());
    assert_eq!(read(&root.join("01-inbox/tea.md")), "# Tea\nchanged in Obsidian\n");
}

#[test]
fn test_create_names_the_page_from_its_title_and_never_overwrites() {
    let (_t, root) = notes_vault();
    let a = create_note(&root, "03-projects/garden", "Seed list", "- tomatoes").unwrap();
    let b = create_note(&root, "03-projects/garden", "Seed list", "").unwrap();
    assert!(a["path"] == "03-projects/garden/seed-list.md" && b["path"] == "03-projects/garden/seed-list-2.md");
    assert_eq!(read(&root.join(s(&a["path"]))), "# Seed list\n\n- tomatoes\n");
    assert_eq!(read(&root.join(s(&b["path"]))), "# Seed list\n");
    assert_eq!(create_note(&root, "01-inbox", "", "").unwrap()["path"], "01-inbox/untitled.md");
    for folder in ["../outside", ".eve-trash", "/tmp", "03-projects/a/b", "nope"] {
        assert!(matches!(create_note(&root, folder, "x", ""), Err(BrainError::Value(_))), "{folder}");
    }
    assert!(!root.parent().unwrap().join("outside").exists());
}

#[test]
fn test_delete_goes_to_the_hidden_trash_and_restore_puts_it_back() {
    let (_t, root) = notes_vault();
    let note = rn(&root, "01-inbox/tea.md");
    let gone = trash_note(&root, "01-inbox/tea.md", s(&note["revision"])).unwrap();
    assert!(ok(&gone) && !s(&gone["trash_id"]).is_empty());
    assert!(!root.join("01-inbox/tea.md").exists());
    assert!(!list_notes(&root).unwrap().iter().any(|n| n.path == "01-inbox/tea.md"));
    assert!(search(&root, "oolong", MAX_HITS, false).unwrap().is_empty()); // the trash is not searched
    assert_eq!(rn(&root, &format!(".eve-trash/{}/tea.md", s(&gone["trash_id"])))["ok"], json!(false));
    // a new note took the name meanwhile: it keeps it, and the restored one is -2
    write(&root.join("01-inbox/tea.md"), "# Other tea\n");
    let back = restore_note(&root, s(&gone["trash_id"])).unwrap();
    assert!(ok(&back) && back["path"] == "01-inbox/tea-2.md");
    assert!(read(&root.join("01-inbox/tea-2.md")).contains("buy oolong"));
    assert_eq!(restore_note(&root, s(&gone["trash_id"])).unwrap_err(), BrainError::Value("nothing to restore".into()));
    assert!(restore_note(&root, "../../x").is_err());
}

#[test]
fn test_move_keeps_the_note_as_it_is() {
    let (_t, root) = notes_vault();
    let before = read(&root.join("01-inbox/tea.md"));
    let out = move_note(&root, "01-inbox/tea.md", "09-archive").unwrap();
    assert_eq!(out, json!({"ok": true, "path": "09-archive/tea.md", "changed": true}));
    assert_eq!(read(&root.join("09-archive/tea.md")), before); // no status: filed
    assert_eq!(move_note(&root, "09-archive/tea.md", "09-archive").unwrap()["changed"], json!(false));
    assert!(move_note(&root, "09-archive/tea.md", "../x").is_err());
}

#[test]
fn test_paths_that_leave_the_vault_or_are_hidden_are_refused() {
    let (_t, root) = notes_vault();
    let secret = root.parent().unwrap().join("secret.md");
    write(&secret, "# no\n");
    for path in ["../secret.md", "/etc/hosts", "~/x.md", ".obsidian/app.json", ".eve-history/x.md", ""] {
        assert_eq!(rn(&root, path)["ok"], json!(false), "{path}");
        assert!(write_note(&root, path, "r", "x").is_err(), "{path}");
        assert!(trash_note(&root, path, "r").is_err(), "{path}");
        assert!(move_note(&root, path, "01-inbox").is_err(), "{path}");
    }
    assert_eq!(read(&secret), "# no\n");
}

#[test]
fn test_an_emptied_body_leaves_the_frontmatter() {
    let (_t, root) = notes_vault();
    let note = rn(&root, "01-inbox/tea.md");
    let out = write_note(&root, "01-inbox/tea.md", s(&note["revision"]), "").unwrap();
    assert_eq!(out["changed"], json!(true));
    assert_eq!(read(&root.join("01-inbox/tea.md")), "---\ncreated: 2026-10-01\nsource: telegram\n---\n");
}

#[test]
fn test_a_save_writes_unix_newlines() {
    for body in ["a\r\nb\r\n", "a\rb"] {
        let (_t, root) = notes_vault();
        let note = rn(&root, "03-projects/garden/beds.md");
        write_note(&root, "03-projects/garden/beds.md", s(&note["revision"]), body).unwrap();
        assert_eq!(read(&root.join("03-projects/garden/beds.md")), "a\nb\n");
    }
}

#[test]
fn test_a_crlf_note_reads_as_python_reads_it() {
    // Python's text mode folds \r\n to \n on read, so the revision is of the folded text
    let (_t, root) = vault();
    write(&root.join("05-resources/win.md"), "# Win\r\nline\r\n");
    let note = rn(&root, "05-resources/win.md");
    assert_eq!(note["text"], "# Win\nline\n");
    assert_eq!(note["revision"], json!(revision("# Win\nline\n")));
}
