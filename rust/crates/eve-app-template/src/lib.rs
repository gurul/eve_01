//! `eve-bridge app new <name>` and `eve-bridge app list` (docs/adding-an-app.md). Port of
//! `eve_bridge/app_template/__init__.py`.
//!
//! [`scaffold`] writes a new app that passes its own tests as generated: the module
//! (`bridge/src/eve_bridge/<name>.py`, an on/off switch kept in memory, two tools and a generic phone sheet), its test
//! (`bridge/tests/test_<name>.py`), its doc (`docs/<name>.md`), and one line in `app_registry.APPS` above the marker.
//! The app is off by default (`EVE_<NAME>=1` turns it on), so writing one changes nothing the models read. A refused
//! name writes nothing.
//!
//! The app it writes is a Python app for the Python daemon, from the same templates, byte for byte: the templates
//! stay in `bridge/src/eve_bridge/app_template/` and are embedded here.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

mod cli;
pub mod pyast;
pub mod template;

pub use cli::{cli, cli_in};
pub use pyast::ParseError;
pub use template::TemplateError;

/// `module.py.tmpl`
pub const MODULE_TEMPLATE: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/src/eve_bridge/app_template/module.py.tmpl"));
/// `test.py.tmpl`
pub const TEST_TEMPLATE: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/src/eve_bridge/app_template/test.py.tmpl"));
/// `doc.md.tmpl`
pub const DOC_TEMPLATE: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/src/eve_bridge/app_template/doc.md.tmpl"));

/// The new app's line in `app_registry.APPS`, a `string.Template`.
pub const REGISTRY_LINE: &str = concat!(
    "    # $name: set symbol (an SF Symbol) and detail (the phone row's subtitle); if you change its tools, change\n",
    "    # acting and clean to match (docs/adding-an-app.md, \"Change the tools\")\n",
    "    App(slot=\"$name\", module=\"$name\", make=\"make\", switch=\"$SWITCH\", default_on=False,\n",
    "        title=\"$Name\", symbol=\"square.grid.2x2\", detail=\"\", doc=\"docs/$name.md\",\n",
    "        no_control=\"$Name is off on this Mac ($SWITCH).\", label=\"$name\", tool_prefix=\"${name}_\",\n",
    "        acting=frozenset({\"${name}_set\"}), clean=frozenset({\"${name}_status\"})),\n",
);

/// Why nothing (or, for an I/O failure mid-write, not everything) was written.
#[derive(Debug, thiserror::Error)]
pub enum ScaffoldError {
    /// Python's `Refused`: the name cannot be a new app; nothing was written.
    #[error("{0}")]
    Refused(String),
    /// Python's `str.index` failing: the marker is in the registry but not at four spaces' indent.
    #[error("{0}")]
    Value(String),
    /// A template placeholder without a value (or an invalid one).
    #[error(transparent)]
    Template(#[from] TemplateError),
    /// bridge/tools/intent_eval.py does not parse.
    #[error(transparent)]
    Syntax(#[from] ParseError),
    /// Reading or writing a file failed.
    #[error("{0}")]
    Io(String),
}

fn refused(msg: impl Into<String>) -> ScaffoldError {
    ScaffoldError::Refused(msg.into())
}

fn io_error(path: &Path, e: std::io::Error) -> ScaffoldError {
    ScaffoldError::Io(format!("{}: {e}", path.display()))
}

/// `Path.read_text(encoding="utf-8")`: universal newlines, so `\r\n` and `\r` read as `\n`.
fn read_text(path: &Path) -> Result<String, ScaffoldError> {
    let text = std::fs::read_to_string(path).map_err(|e| io_error(path, e))?;
    Ok(if text.contains('\r') { text.replace("\r\n", "\n").replace('\r', "\n") } else { text })
}

/// `Path.resolve()`: absolute, symlinks followed as far as the path exists.
fn resolve(path: &Path) -> PathBuf {
    if let Ok(p) = std::fs::canonicalize(path) {
        return p;
    }
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map(|c| c.join(path)).unwrap_or_else(|_| path.to_path_buf())
    };
    // the longest existing ancestor resolved, the rest appended
    let mut tail = Vec::new();
    let mut head = abs.as_path();
    while let Some(parent) = head.parent() {
        if let Some(name) = head.file_name() {
            tail.push(name.to_os_string());
        }
        if let Ok(base) = std::fs::canonicalize(parent) {
            let mut out = base;
            for part in tail.iter().rev() {
                out.push(part);
            }
            return out;
        }
        head = parent;
    }
    abs
}

/// The repository root: the first folder up from `start` (the working folder when None) with
/// bridge/pyproject.toml.
pub fn find_root(start: Option<&Path>) -> Result<PathBuf, ScaffoldError> {
    let here = match start {
        Some(s) => resolve(s),
        None => resolve(&std::env::current_dir().map_err(|e| ScaffoldError::Io(e.to_string()))?),
    };
    for folder in here.ancestors() {
        if folder.join("bridge").join("pyproject.toml").is_file() {
            return Ok(folder.to_path_buf());
        }
    }
    Err(refused("run this inside eve's repository (no bridge/pyproject.toml above this folder)"))
}

/// eve's own tool names in the tool gate's sets (ACTING, CLEAN, _SOURCES, _SAY_HEADS).
fn gate_names() -> BTreeSet<String> {
    let mut names: BTreeSet<String> = eve_tool_gate::ACTING.iter().cloned().collect();
    names.extend(eve_tool_gate::CLEAN.iter().cloned());
    names.extend(eve_tool_gate::SOURCES.keys().cloned());
    names.extend(eve_tool_gate::SAY_HEADS.keys().cloned());
    names
}

/// The first word of each of eve's own tools (the tool gate's sets), so a new app's tools never share one.
pub fn own_prefixes() -> BTreeSet<String> {
    let apps: BTreeSet<&str> = eve_app_registry::APPS
        .iter()
        .flat_map(|app| {
            app.acting
                .iter()
                .chain(app.clean.iter())
                .chain(app.sources.keys())
                .chain(app.say.keys())
                .map(String::as_str)
        })
        .collect();
    gate_names()
        .into_iter()
        .filter(|n| !apps.contains(n.as_str()) && n.contains('_'))
        .map(|n| format!("{}_", n.split('_').next().unwrap_or_default()))
        .collect()
}

/// eve's own tool names (the tool gate's sets), whole: a new app's name must not be one (`screenshot`).
pub fn own_names() -> BTreeSet<String> {
    gate_names()
}

/// Every label the intent eval knows (its LABELS, TOOL_LABELS and CODE_ACTING), read from
/// bridge/tools/intent_eval.py without importing it; empty when the file is not there.
pub fn eval_labels(root: &Path) -> Result<BTreeSet<String>, ScaffoldError> {
    let path = root.join("bridge").join("tools").join("intent_eval.py");
    if !path.is_file() {
        return Ok(BTreeSet::new());
    }
    Ok(pyast::labels_in(&read_text(&path)?, pyast::LABEL_NAMES)?)
}

/// Where the app `name` goes under `root`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub module: PathBuf,
    pub test: PathBuf,
    pub doc: PathBuf,
    pub registry: PathBuf,
}

/// `paths(name, root)`: the module, its test, its doc and the registry it is listed in.
pub fn paths(name: &str, root: &Path) -> Paths {
    Paths {
        module: root.join("bridge").join("src").join("eve_bridge").join(format!("{name}.py")),
        test: root.join("bridge").join("tests").join(format!("test_{name}.py")),
        doc: root.join("docs").join(format!("{name}.md")),
        registry: root.join("bridge").join("src").join("eve_bridge").join("app_registry.py"),
    }
}

fn relative(path: &Path, root: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).display().to_string()
}

/// Write the app `name` under `root` (found from the working folder when None); the paths it wrote, or would write
/// with `dry_run`. Fails with [`ScaffoldError::Refused`], nothing written, for a bad or taken name.
pub fn scaffold(name: &str, root: Option<&Path>, dry_run: bool) -> Result<Vec<PathBuf>, ScaffoldError> {
    let root = match root {
        Some(r) => r.to_path_buf(),
        None => find_root(None)?,
    };
    let prefixes = own_prefixes();
    let mut taken_names = eval_labels(&root)?;
    taken_names.extend(own_names());
    let mut problem = eve_app_registry::name_problem(name, &prefixes, &taken_names);
    let place = paths(name, &root);
    if problem.is_empty() {
        if let Some(taken) = [&place.module, &place.test, &place.doc].into_iter().find(|p| p.exists()) {
            problem = format!("{} already exists", relative(taken, &root));
        }
    }
    let registry = if place.registry.is_file() { read_text(&place.registry)? } else { String::new() };
    if problem.is_empty() && !registry.contains(eve_app_registry::MARKER) {
        problem = format!("the marker line is missing from {}", relative(&place.registry, &root));
    }
    if problem.is_empty() && registry.contains(&format!("slot=\"{name}\"")) {
        problem = format!("{name} is already in the registry");
    }
    if !problem.is_empty() {
        return Err(refused(problem));
    }
    let mut first = name.chars();
    let cap: String = first.next().map(|c| c.to_uppercase().chain(first).collect()).unwrap_or_default();
    let words: HashMap<&str, String> =
        HashMap::from([("name", name.to_string()), ("Name", cap), ("SWITCH", format!("EVE_{}", name.to_uppercase()))]);
    let marker_at = registry
        .find(&format!("    {}", eve_app_registry::MARKER))
        .ok_or_else(|| ScaffoldError::Value("substring not found".to_string()))?;
    let out: Vec<(PathBuf, String)> = vec![
        (place.module.clone(), template::substitute(MODULE_TEMPLATE, &words)?),
        (place.test.clone(), template::substitute(TEST_TEMPLATE, &words)?),
        (place.doc.clone(), template::substitute(DOC_TEMPLATE, &words)?),
        (
            place.registry.clone(),
            format!(
                "{}{}{}",
                &registry[..marker_at],
                template::substitute(REGISTRY_LINE, &words)?,
                &registry[marker_at..]
            ),
        ),
    ];
    if dry_run {
        return Ok(out.into_iter().map(|(p, _)| p).collect());
    }
    for (path, text) in &out {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io_error(parent, e))?;
        }
        std::fs::write(path, text).map_err(|e| io_error(path, e))?;
    }
    Ok(out.into_iter().map(|(p, _)| p).collect())
}

#[cfg(test)]
mod tests;
