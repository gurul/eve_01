//! Port of `eve_bridge.meet`: eve joins a Google Meet call as a silent notetaker, reads the captions, and texts
//! the owner the notes.
//!
//! [`meet`] is the module itself (links, the caption merger, the calendar, the page, the notes, one call's state
//! machine and the owner's side); [`py`] holds the few Python library semantics it rests on (`difflib`'s
//! matching blocks, `urllib.parse`, `format(x, "g")`, `float()`/`int()` of a JSON value).

pub mod meet;
pub mod py;

pub use meet::*;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_ref;
