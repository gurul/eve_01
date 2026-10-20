//! Port of `apps_mcp.py` (the owner's direct apps: Gmail, Calendar, Drive and Contacts over Google's REST APIs,
//! GitHub and Maps over MCP) and `github_repos.py` (the owner's repositories, shown to the brain and opening
//! GitHub by name).

pub mod apps_mcp;
pub mod github_repos;
pub mod py;

pub use apps_mcp::*;
