//! eve's brain: a port of nine modules of eve's Python daemon (`bridge/src/eve_bridge`), 15446 lines.
//!
//! * [`telegram`]: the Telegram door and the text brain (TelegramInlet), also the brain of Eve Link's chat.
//! * [`voice_agent`]: the voice session over the Live API (VoiceSession).
//! * [`think`]: think_hard, the slow careful brain.
//! * [`miniapp`]: the Mini App server and its Cloudflare tunnel (apps, Eve Link, calls, spend).
//! * [`apps_maker`]: the apps eve builds for the owner (the store, the maker, the chat maker).
//! * [`share`]: sending a file to someone (iMessage, Gmail, Drive).
//! * [`notify`]: phone notifications through ntfy.
//! * [`onboarding`]: OpenRouter and Composio Connect sign-in (OAuth PKCE).
//! * [`self_context`]: what eve runs on, told to its own brains.
//!
//! Python's module globals that tests patch are parameters or seams here; see PARITY.md.

// Skeleton: the bodies are todo!() until their fill units port them. Integration removes this allow.
#![allow(unused_variables, dead_code, unused_imports, clippy::all)]

pub mod apps_maker;
pub mod miniapp;
pub mod notify;
pub mod onboarding;
pub mod prelude;
pub mod seams;
pub mod self_context;
pub mod share;
pub mod telegram;
pub mod think;
pub mod voice_agent;
