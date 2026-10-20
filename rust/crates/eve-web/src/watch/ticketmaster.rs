//! watch.py's Ticketmaster rules (pure): which attraction the owner means, which events are tribute acts, and the
//! reading from Discovery API events. The two reads that call the API (`_tm_get`, `_read_ticketmaster`) are the
//! Watcher's, in watcher.rs.

use serde_json::{Map, Value};

use super::page::Reading;

/// `TRIBUTE`.
pub const TRIBUTE: &str = r"(?i)\b(tribute|a tribute to|experience|salute to|legacy of|vs\.?|cover band|celebrating)\b";

/// `_iso(raw)`: an ISO time (a trailing Z is UTC) as epoch seconds, None when it is not one.
pub fn iso(raw: &Value) -> Option<f64> {
    let _ = raw;
    todo!("watch._iso")
}

/// `_tribute_words(text)`: the tribute words in `text`, lowercased, "a " and " to" and a trailing "." dropped.
pub fn tribute_words(text: &str) -> std::collections::HashSet<String> {
    let _ = text;
    todo!("watch._tribute_words")
}

/// `_tribute(name, query, sports=)`: a tribute act the owner did not ask for: the name has a tribute word the
/// owner's own words lack. A game is billed "A vs. B", so "vs" marks no tribute in the Sports segment.
pub fn tribute(name: &str, query: &str, sports: bool) -> bool {
    let _ = (name, query, sports);
    todo!("watch._tribute")
}

/// `_sports(event)`: one of its classifications is the Sports segment.
pub fn sports(event: &Map<String, Value>) -> bool {
    let _ = event;
    todo!("watch._sports")
}

/// `_norm(name)`: lowercased, punctuation to spaces, whitespace collapsed.
pub fn norm(name: &str) -> String {
    let _ = name;
    todo!("watch._norm")
}

/// `_tm_events(payload)`: `_embedded.events`, objects only.
pub fn tm_events(payload: &Map<String, Value>) -> Vec<Map<String, Value>> {
    let _ = payload;
    todo!("watch._tm_events")
}

/// The attraction the owner means: its name or an alias equals the query (case and punctuation aside), it is not
/// a tribute act, and among several the one with the most upcoming events. "" when none is.
pub fn pick_attraction(query: &str, payload: &Map<String, Value>) -> String {
    let _ = (query, payload);
    todo!("watch.pick_attraction")
}

/// A reading from Discovery API events, by Ticketmaster's own status definitions: an event is available when it is
/// onsale or rescheduled, or when one of its presale windows is open now, whatever its status; an offsale event
/// whose public sale has ended is dropped; postponed and canceled are not on sale. priceRanges was removed from
/// Ticketmaster's feeds on 2025-03-11: when it is absent the price is unknown (None), never 0. The note counts the
/// dates and says when the next sale opens.
pub fn ticketmaster_reading(events: &[Map<String, Value>], now: f64) -> Reading {
    let _ = (events, now);
    todo!("watch.ticketmaster_reading")
}
