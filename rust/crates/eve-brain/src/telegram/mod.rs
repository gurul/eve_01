//! The Telegram door and eve's text brain: a port of `telegram.py` (7699 lines), cut by section.
//!
//! * [`prompts`], [`tools`], [`config`]: the constants, the text brain's instructions and tools, the config.
//! * [`accept`], [`brain`], [`bot_api`]: an update accepted or dropped, the request and its parse, the Bot API.
//! * [`inlet`]: `TelegramInlet`, its state in `inlet/mod.rs` and its methods in one `impl` block per section file.
//! * [`make`]: `diagnose`, `MutedBotApi`, `make_inlet`.
//!
//! Architect-owned: no fill unit edits this file.

pub mod accept;
pub mod bot_api;
pub mod brain;
pub mod config;
pub mod inlet;
pub mod make;
pub mod prompts;
pub mod tools;

pub use accept::*;
pub use bot_api::*;
pub use brain::*;
pub use config::*;
pub use inlet::*;
pub use make::*;
pub use prompts::*;
pub use tools::*;

/// `from .telegram_format import MAX_MESSAGE_CHARS, plain`: the names other modules and tests take from here.
pub use eve_telegram_format::{plain, MAX_MESSAGE_CHARS};
