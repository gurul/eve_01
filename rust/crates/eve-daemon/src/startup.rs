//! `code_origin()` and `_log_permission_config_summary(matchers)`: what the daemon logs as it starts.

/// `code_origin()`: where the running code came from, for the start-up log line: the checkout's branch and short
/// commit (`git -C <package dir> ...`), or "installed package" outside a checkout.
pub fn code_origin() -> String {
    todo!()
}

/// `_log_permission_config_summary(matchers)`: one INFO line per matcher class, and the Claude config homes the
/// hooks are installed in (claude_home.claude_config_dirs).
pub fn log_permission_config_summary(matchers: &eve_matchers::MatcherConfig) {
    todo!()
}
