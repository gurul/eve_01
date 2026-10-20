//! Port of bridge/tests/test_link_notes.py: the Notes app over a vault in a temp dir. Every action, the off state,
//! frontmatter kept on a save, a stale revision refused, delete and restore, and paths that leave the vault. Plus
//! the Notes half of test_link_chat.py's controls listing (`available`). The words here are made up.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use eve_second_brain as sb;
use serde_json::{json, Map, Value};

use super::*;

fn write(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
}

fn read_file(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

fn vault(tmp: &Path) -> PathBuf {
    let root = tmp.join("vault");
    sb::ensure_vault(&root).unwrap();
    write(
        &root.join("01-inbox").join("tea.md"),
        "---\ncreated: 2026-10-01\nsource: telegram\n---\n# Tea\n\n- [ ] buy oolong\n- [x] boil water\n",
    );
    fs::create_dir(root.join("03-projects").join("garden")).unwrap();
    write(&root.join("03-projects").join("garden").join("beds.md"), "# Beds\nRaised beds by the fence.\n");
    // the newest
    let f = fs::File::options().write(true).open(root.join("01-inbox").join("tea.md")).unwrap();
    f.set_modified(UNIX_EPOCH + Duration::from_secs(2_000_000_000)).unwrap();
    root
}

fn panel_with(root: &Path, enabled: bool) -> NotesPanel {
    let root = root.to_path_buf();
    NotesPanel::new(Arc::new(move || VaultConfig { enabled, root: root.clone() }))
}

fn panel(root: &Path) -> NotesPanel {
    panel_with(root, true)
}

async fn run(p: &NotesPanel, body: Value) -> Value {
    let body: Map<String, Value> = body.as_object().cloned().unwrap();
    let out = p.panel(&body).await.unwrap();
    serde_json::to_string(&out).unwrap(); // always JSON, as link_chat sends it
    out
}

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[tokio::test]
async fn test_off_or_missing_vault_is_unavailable_and_says_so() {
    let t = tmp();
    let root = vault(t.path());
    for p in [panel_with(&root, false), panel(&t.path().join("nowhere"))] {
        assert!(!p.available());
        assert_eq!(run(&p, json!({"action": "list"})).await, json!({"ok": false, "line": OFF_LINE}));
    }
    assert!(panel(&root).available());
}

#[tokio::test]
async fn test_list_is_every_note_newest_first_with_folders() {
    let t = tmp();
    let root = vault(t.path());
    let out = run(&panel(&root), json!({})).await;
    assert_eq!(out["ok"], json!(true));
    assert_eq!(
        out["notes"][0],
        json!({"path": "01-inbox/tea.md", "folder": "01-inbox", "title": "Tea", "preview": "buy oolong",
               "modified": 2_000_000_000i64})
    );
    let paths: Vec<&str> = out["notes"].as_array().unwrap().iter().map(|n| n["path"].as_str().unwrap()).collect();
    assert!(paths.contains(&"03-projects/garden/beds.md") && paths.contains(&"CLAUDE.md"));
    assert!(!paths.iter().any(|p| p.starts_with('.') || p.contains("/.")));
    let folders: Vec<&str> = out["folders"].as_array().unwrap().iter().map(|f| f.as_str().unwrap()).collect();
    assert!(folders.contains(&"03-projects/garden"));
    assert_eq!(&folders[..2], &["00-vision", "01-inbox"]);
    assert_eq!(out["line"], json!(""));
}

#[tokio::test]
async fn test_a_captures_preview_is_not_its_title_again() {
    let t = tmp();
    let root = vault(t.path());
    write(&root.join("01-inbox").join("cap.md"), "---\nkind: note\n---\nThe blue cafe is good\n\nGo on a Tuesday\n");
    write(&root.join("01-inbox").join("one.md"), "Only a line\n");
    let out = run(&panel(&root), json!({"action": "list"})).await;
    let row = |path: &str| out["notes"].as_array().unwrap().iter().find(|n| n["path"] == json!(path)).unwrap().clone();
    assert_eq!(row("01-inbox/cap.md")["title"], json!("The blue cafe is good"));
    assert_eq!(row("01-inbox/cap.md")["preview"], json!("Go on a Tuesday"));
    assert_eq!(row("01-inbox/one.md")["preview"], json!(""));
    assert_eq!(row("03-projects/garden/beds.md")["preview"], json!("Raised beds by the fence."));
}

#[tokio::test]
async fn test_read_splits_the_frontmatter_off_and_save_keeps_it() {
    let t = tmp();
    let root = vault(t.path());
    let p = panel(&root);
    let note = run(&p, json!({"action": "read", "path": "01-inbox/tea.md"})).await;
    assert_eq!(note["ok"], json!(true));
    assert_eq!(note["title"], json!("Tea"));
    assert_eq!(note["editable"], json!(true));
    assert_eq!(note["front"], json!(["created: 2026-10-01", "source: telegram"]));
    let text = note["body"].as_str().unwrap();
    assert!(text.starts_with("# Tea\n") && !text.contains("---"));
    let body = text.replace("- [ ] buy oolong", "- [x] buy oolong");
    let saved =
        run(&p, json!({"action": "save", "path": "01-inbox/tea.md", "revision": note["revision"], "body": body})).await;
    assert_eq!(saved["ok"], json!(true));
    assert_eq!(saved["changed"], json!(true));
    assert!(saved["undo_id"].as_str().is_some_and(|s| !s.is_empty()));
    let on_disk = read_file(&root.join("01-inbox").join("tea.md"));
    assert!(on_disk.starts_with("---\ncreated: 2026-10-01\nsource: telegram\n---\n# Tea") && on_disk.contains("- [x] buy oolong"));
    // the same body again changes nothing
    let again =
        run(&p, json!({"action": "save", "path": "01-inbox/tea.md", "revision": saved["revision"], "body": body})).await;
    assert_eq!(again["ok"], json!(true));
    assert_eq!(again["changed"], json!(false));
    // and undo takes the save back
    let undone = run(
        &p,
        json!({"action": "undo", "path": "01-inbox/tea.md", "revision": saved["revision"], "undo_id": saved["undo_id"]}),
    )
    .await;
    assert_eq!(undone["ok"], json!(true));
    assert_eq!(undone["line"], json!("Undone."));
    assert!(read_file(&root.join("01-inbox").join("tea.md")).contains("- [ ] buy oolong"));
}

#[tokio::test]
async fn test_a_stale_revision_is_refused_and_nothing_is_written() {
    let t = tmp();
    let root = vault(t.path());
    let p = panel(&root);
    let note = run(&p, json!({"action": "read", "path": "01-inbox/tea.md"})).await;
    write(&root.join("01-inbox").join("tea.md"), "# Tea\nchanged in Obsidian\n");
    for body in [
        json!({"action": "save", "path": "01-inbox/tea.md", "revision": note["revision"], "body": "mine"}),
        json!({"action": "delete", "path": "01-inbox/tea.md", "revision": note["revision"]}),
        json!({"action": "save", "path": "01-inbox/tea.md", "revision": "", "body": "mine"}),
    ] {
        assert_eq!(run(&p, body.clone()).await, json!({"ok": false, "line": CHANGED_LINE}), "{body}");
    }
    assert_eq!(read_file(&root.join("01-inbox").join("tea.md")), "# Tea\nchanged in Obsidian\n");
}

#[tokio::test]
async fn test_create_names_the_page_from_its_title_and_never_overwrites() {
    let t = tmp();
    let root = vault(t.path());
    let p = panel(&root);
    let a = run(&p, json!({"action": "create", "folder": "03-projects/garden", "title": "Seed list", "body": "- tomatoes"}))
        .await;
    let b = run(&p, json!({"action": "create", "folder": "03-projects/garden", "title": "Seed list"})).await;
    assert_eq!(a["path"], json!("03-projects/garden/seed-list.md"));
    assert_eq!(b["path"], json!("03-projects/garden/seed-list-2.md"));
    assert_eq!(read_file(&root.join(a["path"].as_str().unwrap())), "# Seed list\n\n- tomatoes\n");
    assert_eq!(read_file(&root.join(b["path"].as_str().unwrap())), "# Seed list\n");
    // the inbox by default
    assert_eq!(run(&p, json!({"action": "create", "title": ""})).await["path"], json!("01-inbox/untitled.md"));
    for folder in ["../outside", ".eve-trash", "/tmp", "03-projects/a/b", "nope"] {
        let out = run(&p, json!({"action": "create", "folder": folder, "title": "x"})).await;
        assert_eq!(out, json!({"ok": false, "line": "That isn't one of your note folders."}), "{folder}");
    }
    assert!(!t.path().join("outside").exists());
}

#[tokio::test]
async fn test_delete_goes_to_the_hidden_trash_and_restore_puts_it_back() {
    let t = tmp();
    let root = vault(t.path());
    let p = panel(&root);
    let note = run(&p, json!({"action": "read", "path": "01-inbox/tea.md"})).await;
    let gone = run(&p, json!({"action": "delete", "path": "01-inbox/tea.md", "revision": note["revision"]})).await;
    assert_eq!(gone["ok"], json!(true));
    assert_eq!(gone["line"], json!("Deleted."));
    let trash_id = gone["trash_id"].as_str().unwrap().to_string();
    assert!(!trash_id.is_empty());
    assert!(!root.join("01-inbox").join("tea.md").exists());
    let listed = run(&p, json!({"action": "list"})).await;
    assert!(!listed["notes"].as_array().unwrap().iter().any(|n| n["path"] == json!("01-inbox/tea.md")));
    // the trash is not searched
    assert_eq!(run(&p, json!({"action": "search", "query": "oolong"})).await["hits"], json!([]));
    assert_eq!(run(&p, json!({"action": "read", "path": format!(".eve-trash/{trash_id}/tea.md")})).await["ok"], json!(false));
    // a new note took the name meanwhile: it keeps it, and the restored one is -2
    write(&root.join("01-inbox").join("tea.md"), "# Other tea\n");
    let back = run(&p, json!({"action": "restore", "trash_id": trash_id})).await;
    assert_eq!(back, json!({"ok": true, "path": "01-inbox/tea-2.md", "line": "Restored."}));
    assert!(read_file(&root.join("01-inbox").join("tea-2.md")).contains("buy oolong"));
    assert_eq!(
        run(&p, json!({"action": "restore", "trash_id": trash_id})).await,
        json!({"ok": false, "line": "There's nothing to restore."})
    );
    assert_eq!(run(&p, json!({"action": "restore", "trash_id": "../../x"})).await["ok"], json!(false));
}

#[tokio::test]
async fn test_move_keeps_the_note_as_it_is() {
    let t = tmp();
    let root = vault(t.path());
    let p = panel(&root);
    let before = read_file(&root.join("01-inbox").join("tea.md"));
    let out = run(&p, json!({"action": "move", "path": "01-inbox/tea.md", "into": "09-archive"})).await;
    assert_eq!(out, json!({"ok": true, "path": "09-archive/tea.md", "changed": true, "line": "Moved."}));
    assert_eq!(read_file(&root.join("09-archive").join("tea.md")), before); // no status: filed
    let same = run(&p, json!({"action": "move", "path": "09-archive/tea.md", "into": "09-archive"})).await;
    assert_eq!(same, json!({"ok": true, "path": "09-archive/tea.md", "changed": false, "line": ""}));
    assert_eq!(run(&p, json!({"action": "move", "path": "09-archive/tea.md", "into": "../x"})).await["ok"], json!(false));
}

#[tokio::test]
async fn test_paths_that_leave_the_vault_or_are_hidden_are_refused() {
    let t = tmp();
    let root = vault(t.path());
    write(&t.path().join("secret.md"), "# no\n");
    let p = panel(&root);
    for path in ["../secret.md", "/etc/hosts", "~/x.md", ".obsidian/app.json", ".eve-history/x.md", ""] {
        for action in ["read", "save", "delete", "move"] {
            let out =
                run(&p, json!({"action": action, "path": path, "revision": "r", "body": "x", "into": "01-inbox"})).await;
            assert_eq!(out["ok"], json!(false), "{action} {path}");
        }
    }
    assert_eq!(read_file(&t.path().join("secret.md")), "# no\n");
    assert_eq!(
        run(&p, json!({"action": "drop table"})).await,
        json!({"ok": false, "line": "That isn't something Notes does."})
    );
}

#[tokio::test]
async fn test_a_note_too_long_for_one_request_is_read_only() {
    let t = tmp();
    let root = vault(t.path());
    write(&root.join("05-resources").join("big.md"), &format!("# Big\n{}", "x".repeat(MAX_BODY_CHARS + 10)));
    let p = panel(&root);
    let note = run(&p, json!({"action": "read", "path": "05-resources/big.md"})).await;
    assert_eq!(note["ok"], json!(true));
    assert_eq!(note["editable"], json!(false));
    let out = run(
        &p,
        json!({"action": "save", "path": "05-resources/big.md", "revision": note["revision"],
               "body": "y".repeat(MAX_BODY_CHARS + 1)}),
    )
    .await;
    assert_eq!(out, json!({"ok": false, "line": TOO_LONG_LINE}));
}

#[tokio::test]
async fn test_search_finds_words_and_says_so_when_none_match() {
    let t = tmp();
    let root = vault(t.path());
    let p = panel(&root);
    let out = run(&p, json!({"action": "search", "query": "fence"})).await;
    assert_eq!(out["ok"], json!(true));
    assert_eq!(out["hits"][0]["path"], json!("03-projects/garden/beds.md"));
    assert_eq!(out["line"], json!(""));
    assert_eq!(
        run(&p, json!({"action": "search", "query": "zebra"})).await,
        json!({"ok": true, "hits": [], "line": "No note has those words."})
    );
    assert_eq!(run(&p, json!({"action": "search", "query": "  "})).await["ok"], json!(false));
}

#[test]
fn test_an_emptied_body_leaves_the_frontmatter() {
    let t = tmp();
    let root = vault(t.path());
    let note = sb::read_note(&root, "01-inbox/tea.md", sb::MAX_NOTE_CHARS).unwrap();
    let out = sb::write_note(&root, "01-inbox/tea.md", note["revision"].as_str().unwrap(), "").unwrap();
    assert_eq!(out["changed"], json!(true));
    assert_eq!(read_file(&root.join("01-inbox").join("tea.md")), "---\ncreated: 2026-10-01\nsource: telegram\n---\n");
}

#[test]
fn test_a_save_writes_unix_newlines() {
    for body in ["a\r\nb\r\n", "a\rb"] {
        let t = tmp();
        let root = vault(t.path());
        let note = sb::read_note(&root, "03-projects/garden/beds.md", sb::MAX_NOTE_CHARS).unwrap();
        sb::write_note(&root, "03-projects/garden/beds.md", note["revision"].as_str().unwrap(), body).unwrap();
        assert_eq!(read_file(&root.join("03-projects").join("garden").join("beds.md")), "a\nb\n", "{body:?}");
    }
}

// ---- added: the Python behavior the tests above do not reach ---------------------------------------

/// test_link_chat.py's controls listing: Notes is there only while the brain is on and its folder exists.
#[test]
fn test_available_follows_the_switch_and_the_folder() {
    let t = tmp();
    assert!(!panel_with(t.path(), false).available());
    assert!(!panel(&t.path().join("missing")).available());
    assert!(panel(t.path()).available());
}

/// A config that raises makes Notes unavailable rather than failing the request.
#[tokio::test]
async fn test_a_config_that_fails_is_off() {
    let p = NotesPanel::new(Arc::new(|| panic!("config exploded")));
    assert!(!p.available());
    assert_eq!(run(&p, json!({"action": "list"})).await, json!({"ok": false, "line": OFF_LINE}));
}

/// `str(body.get("action") or "list")`: a falsy action is a list; a non-string action is not an action.
#[tokio::test]
async fn test_a_falsy_action_lists_and_odd_ones_are_refused() {
    let t = tmp();
    let root = vault(t.path());
    let p = panel(&root);
    for action in [json!(null), json!(""), json!(0), json!(false), json!([])] {
        assert_eq!(run(&p, json!({"action": action})).await["ok"], json!(true), "{action}");
    }
    for action in [json!(1), json!(["list"]), json!({"list": 1}), json!(true)] {
        assert_eq!(
            run(&p, json!({"action": action})).await,
            json!({"ok": false, "line": "That isn't something Notes does."})
        );
    }
}

#[tokio::test]
async fn test_save_without_a_string_body_and_without_a_path() {
    let t = tmp();
    let root = vault(t.path());
    let p = panel(&root);
    let note = run(&p, json!({"action": "read", "path": "01-inbox/tea.md"})).await;
    for body in [json!(null), json!(5), json!(["x"])] {
        assert_eq!(
            run(&p, json!({"action": "save", "path": "01-inbox/tea.md", "revision": note["revision"], "body": body})).await,
            json!({"ok": false, "line": "Nothing to save."})
        );
    }
    for action in ["read", "save", "undo", "delete", "move"] {
        assert_eq!(run(&p, json!({"action": action, "path": "   "})).await, json!({"ok": false, "line": "Which note?"}));
    }
}

#[tokio::test]
async fn test_read_reports_its_fields_and_the_undo_id_after_a_save() {
    let t = tmp();
    let root = vault(t.path());
    let p = panel(&root);
    let note = run(&p, json!({"action": "read", "path": " 01-inbox/tea.md "})).await;
    let keys: Vec<&str> = note.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        ["ok", "path", "title", "body", "front", "revision", "modified", "editable", "undo_id", "line"]
    );
    assert_eq!(note["undo_id"], json!(null));
    assert_eq!(note["modified"], json!(2_000_000_000i64));
    assert_eq!(note["path"], json!("01-inbox/tea.md"));
    let saved =
        run(&p, json!({"action": "save", "path": "01-inbox/tea.md", "revision": note["revision"], "body": "# Tea\n"})).await;
    let again = run(&p, json!({"action": "read", "path": "01-inbox/tea.md"})).await;
    assert_eq!(again["undo_id"], saved["undo_id"]);
    assert_eq!(again["revision"], saved["revision"]);
    // a file that is not markdown is not a note
    write(&root.join("01-inbox").join("x.txt"), "plain\n");
    assert_eq!(
        run(&p, json!({"action": "read", "path": "01-inbox/x.txt"})).await,
        json!({"ok": false, "line": NOT_A_NOTE})
    );
}

#[tokio::test]
async fn test_a_stale_undo_id_cannot_be_undone() {
    let t = tmp();
    let root = vault(t.path());
    let p = panel(&root);
    let note = run(&p, json!({"action": "read", "path": "01-inbox/tea.md"})).await;
    let out =
        run(&p, json!({"action": "undo", "path": "01-inbox/tea.md", "revision": note["revision"], "undo_id": "../x"})).await;
    assert_eq!(out, json!({"ok": false, "line": "That change can't be undone any more."}));
}

#[tokio::test]
async fn test_an_unreadable_vault_is_a_mac_failure() {
    let t = tmp();
    let root = vault(t.path());
    let p = panel(&root);
    // a file where the destination folder should be: mkdir raises FileExistsError, an OSError, not a refusal
    write(&root.join("04-areas").join("home"), "not a folder\n");
    let out = run(&p, json!({"action": "create", "folder": "04-areas/home", "title": "x"})).await;
    assert_eq!(out, json!({"ok": false, "line": "That didn't work on the Mac just now."}));
}

#[test]
fn test_owner_line_says_each_reason_to_the_owner() {
    assert_eq!(owner_line("note changed; read_note again before editing"), CHANGED_LINE);
    assert_eq!(owner_line("no such markdown note"), NOT_A_NOTE);
    assert_eq!(owner_line("nothing to restore"), "There's nothing to restore.");
    assert_eq!(owner_line("not a vault folder: x"), "That isn't one of your note folders.");
    assert_eq!(owner_line("use an undo_id returned by an edit"), "That change can't be undone any more.");
    assert_eq!(owner_line("invalid note history"), "That change can't be undone any more.");
    assert_eq!(owner_line("path leaves the vault"), NOT_A_NOTE);
    assert_eq!(owner_line("hidden paths are off limits"), NOT_A_NOTE);
    assert_eq!(owner_line("something else"), "That didn't work: something else");
}
