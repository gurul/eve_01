//! watch.py's chat tools: their schemas and the instructions block, byte for byte. Data only.

use std::sync::LazyLock;

use serde_json::Value;

/// `TOOLS`: the six function tools (watch_add, watch_list, watch_remove, watch_pause, watch_resume,
/// watch_set_end), as Python's list of dicts (key order kept).
pub static TOOLS: LazyLock<Value> = LazyLock::new(|| serde_json::from_str(TOOLS_JSON).expect("watch TOOLS"));

const TOOLS_JSON: &str = r#"[
 {
  "type": "function",
  "name": "watch_add",
  "strict": true,
  "description": "Start watching a price, a stock or crypto quote, a page, or a release, and text the owner when the condition happens. Runs a first check now and returns the current reading.",
  "parameters": {
   "type": "object",
   "additionalProperties": false,
   "required": [
    "kind",
    "target",
    "label",
    "condition",
    "value",
    "text",
    "city",
    "every_minutes",
    "for_hours"
   ],
   "properties": {
    "kind": {
     "type": "string",
     "enum": [
      "quote",
      "page",
      "search",
      "ticketmaster"
     ]
    },
    "target": {
     "type": "string",
     "description": "quote: the Yahoo Finance symbol. page: the full link. search: a search question naming exactly what, where and when. ticketmaster: the artist, team or show, or a ticketmaster.com event link."
    },
    "label": {
     "type": "string",
     "description": "A short name the owner will recognise."
    },
    "condition": {
     "type": "string",
     "enum": [
      "below",
      "above",
      "drop_pct",
      "rise_pct",
      "change",
      "available",
      "appears"
     ]
    },
    "value": {
     "type": [
      "number",
      "null"
     ],
     "description": "below/above: the price. drop_pct/rise_pct: the percentage. Otherwise null."
    },
    "text": {
     "type": [
      "string",
      "null"
     ],
     "description": "appears: the phrase. Otherwise null."
    },
    "city": {
     "type": [
      "string",
      "null"
     ],
     "description": "ticketmaster only: the city, when the owner named one."
    },
    "every_minutes": {
     "type": [
      "number",
      "null"
     ],
     "description": "How often to check, when the owner said; else null."
    },
    "for_hours": {
     "type": [
      "number",
      "null"
     ],
     "description": "How long to keep watching, in hours from now, when the owner gave a period or an end (\"for a week\" is 168, \"until Friday\" is the hours until then); else null."
    }
   }
  }
 },
 {
  "type": "function",
  "name": "watch_list",
  "strict": true,
  "description": "What eve is watching, with each watch's id, condition and latest reading.",
  "parameters": {
   "type": "object",
   "additionalProperties": false,
   "required": [],
   "properties": {}
  }
 },
 {
  "type": "function",
  "name": "watch_remove",
  "strict": true,
  "description": "Stop watching one thing for good, by the id watch_list or watch_add gave.",
  "parameters": {
   "type": "object",
   "additionalProperties": false,
   "required": [
    "id"
   ],
   "properties": {
    "id": {
     "type": "string"
    }
   }
  }
 },
 {
  "type": "function",
  "name": "watch_pause",
  "strict": true,
  "description": "Pause a watch: no checks and no alerts until it is resumed, or until the hours given run out. The watch, its mark and its last reading are kept.",
  "parameters": {
   "type": "object",
   "additionalProperties": false,
   "required": [
    "id",
    "for_hours"
   ],
   "properties": {
    "id": {
     "type": "string"
    },
    "for_hours": {
     "type": [
      "number",
      "null"
     ],
     "description": "Resume by itself after this many hours; null to stay paused until resumed."
    }
   }
  }
 },
 {
  "type": "function",
  "name": "watch_resume",
  "strict": true,
  "description": "Resume a paused watch; it is checked again right away.",
  "parameters": {
   "type": "object",
   "additionalProperties": false,
   "required": [
    "id"
   ],
   "properties": {
    "id": {
     "type": "string"
    }
   }
  }
 },
 {
  "type": "function",
  "name": "watch_set_end",
  "strict": true,
  "description": "Set when a watch ends by itself (it is removed then, and the owner told), or clear its end.",
  "parameters": {
   "type": "object",
   "additionalProperties": false,
   "required": [
    "id",
    "for_hours"
   ],
   "properties": {
    "id": {
     "type": "string"
    },
    "for_hours": {
     "type": [
      "number",
      "null"
     ],
     "description": "End this many hours from now; null to keep watching with no end."
    }
   }
  }
 }
]"#;

/// `TOOL_NAMES`.
pub const TOOL_NAMES: [&str; 6] = ["watch_add", "watch_list", "watch_remove", "watch_pause", "watch_resume", "watch_set_end"];

pub const INSTRUCTIONS_BLOCK: &str = r#"You can keep watch for your owner and text them when something happens, with watch_add.
Pick the kind by what they gave you:
- a stock, fund, index or crypto price: kind quote, target the symbol as Yahoo Finance writes it (a crypto
  against the dollar is written like BTC-USD).
- a link to a product or ticket page: kind page, target the full link.
- no link, such as tickets for an artist in a city going on sale: kind search, target a search question
  that names the artist or item, the city or store, and the dates.
The condition: below or above a price (value), drop_pct or rise_pct by a percentage (value), change for any
new price, available for in stock or on sale, appears when a phrase shows up on the page (text). Ask one
short question first when the thing to watch or the mark is unclear.
watch_add checks once right away: tell them that reading in a few words and how often you will check. When
the reading already meets the condition, say so plainly. watch_list shows what you are watching; watch_remove
stops one for good by its id.
When the owner wants it only for a while ("for the next week", "until Friday", "during the presale"), pass
for_hours to watch_add, or watch_set_end for a watch they already have; work the hours out from the current
time in your context. "pause" is watch_pause (for_hours when they say for how long) and "resume" or "start
again" is watch_resume: a paused watch keeps its mark and its last reading."#;

pub const TICKETMASTER_LINE: &str = r#"
- tickets for a concert, game or show going on sale: kind ticketmaster, condition available, target the
  artist, team or show (or its ticketmaster.com event link), city when they named one. Prefer it over search
  for on-sale alerts: it knows the real public sale and presale times. It has no reliable prices: for a
  ticket price, watch the event page (kind page)."#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tool_names_are_the_tools() {
        let names: Vec<&str> = TOOLS.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, TOOL_NAMES);
        assert!(TICKETMASTER_LINE.starts_with('\n'));
        assert!(INSTRUCTIONS_BLOCK.contains("\nThe condition:"));
    }
}
