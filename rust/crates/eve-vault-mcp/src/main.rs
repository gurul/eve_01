//! `python -m eve_bridge.vault_mcp`: the vault's stdio MCP server for Codex tasks.

fn main() {
    // stdout is the protocol and nothing else is printed: a panic is answered as a sentence, never a trace.
    std::panic::set_hook(Box::new(|_| {}));
    std::process::exit(eve_vault_mcp::main());
}
