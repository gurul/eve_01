//! `eve-bridge notes-widget --once` against throwaway folders: builds the panel, shows it for one second, prints what
//! it rendered. `cargo run -p eve-notes-widget --example once`.

fn main() -> anyhow::Result<()> {
    let tmp = std::env::temp_dir().join(format!("eve-notes-widget-once-{}", std::process::id()));
    let notes = tmp.join("notes");
    let store = tmp.join("memory");
    std::fs::create_dir_all(&notes)?;
    let today = chrono::Local::now().date_naive().format("%Y-%m-%d").to_string();
    std::fs::write(notes.join(format!("{today}.md")), "- 09:30 yaw=+5 pitch=10 — a mug on the desk\n")?;
    let code = eve_notes_widget::run(true, Some(notes), Some(tmp.join("widget.json")), Some(store))?;
    let _ = std::fs::remove_dir_all(&tmp);
    std::process::exit(code)
}
