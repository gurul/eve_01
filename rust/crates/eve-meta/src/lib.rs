//! eve-bridge: Claude Code CLI ↔ claude-desktop-buddy BLE bridge.
//!
//! Port of `eve_bridge/__init__.py`: the package docstring and `__version__`.

/// The package version, `eve_bridge.__version__`.
///
/// Copied verbatim from the Python package rather than taken from
/// `CARGO_PKG_VERSION`: the Python `__version__` is what `--version`,
/// the update check's `User-Agent` and its "current" comparison report, and the
/// workspace manifest version is not kept in step with it.
pub const VERSION: &str = "0.1.2";

/// The package docstring, `eve_bridge.__doc__`.
pub const DESCRIPTION: &str = "eve-bridge: Claude Code CLI ↔ claude-desktop-buddy BLE bridge.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_matches_python_package() {
        let init = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../bridge/src/eve_bridge/__init__.py"
        ));
        assert!(init.contains(&format!("__version__ = \"{VERSION}\"")));
        assert!(init.contains(&format!("\"\"\"{DESCRIPTION}\"\"\"")));
    }

    #[test]
    fn version_is_dotted_integers() {
        let parts: Vec<u64> = VERSION
            .split('.')
            .map(|p| p.parse().expect("numeric version component"))
            .collect();
        assert_eq!(parts, vec![0, 1, 2]);
    }

    #[test]
    fn cli_version_string_format() {
        assert_eq!(format!("eve-bridge {VERSION}"), "eve-bridge 0.1.2");
        assert_eq!(format!("eve-bridge/{VERSION}"), "eve-bridge/0.1.2");
    }
}
