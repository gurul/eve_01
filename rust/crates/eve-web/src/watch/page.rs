//! watch.py's "a reading and reading pages" section: what one check found, and the page's own structured data
//! (JSON-LD offers, price meta tags, microdata), read with no model.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// What one check found. `value` is a price; `available` whether it can be bought or booked now; `hit` whether
/// the watched phrase is on the page. Any may be None: not known.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Reading {
    pub value: Option<f64>,
    pub currency: String,
    pub available: Option<bool>,
    pub hit: Option<bool>,
    pub note: String,
    pub url: String,
    /// "quote", "structured", "model", "search", "ticketmaster", "text", "vision", "browser", "hosted"
    pub source: String,
    pub name: String,
    /// the vision model saw a bot check, not the page
    pub blocked: bool,
}

/// ISO 4217 currencies with 3 minor digits.
pub const THREE_DECIMALS: [&str; 7] = ["KWD", "BHD", "OMR", "JOD", "TND", "LYD", "IQD"];
/// `_NUMBER` (the class holds a no-break space U+00A0, a narrow no-break space U+202F, an apostrophe and a space).
pub const NUMBER: &str = "\\d[\\d.,\u{a0}\u{202f}' ]*\\d|\\d";
/// `_SIGNED`.
pub const SIGNED: &str = "[$€£¥₹₩₺₽]\\s*(\\d[\\d.,\u{a0}\u{202f}' ]*\\d|\\d)";
/// type="application/ld+json", unquoted as a minifier leaves it, or with parameters ("…; charset=utf-8")
pub const LD_TYPE: &str = r#"(?i)type\s*=\s*["']?\s*application/ld\+json\b"#;
pub const META_RE: &str = r"(?i)<meta\b[^>]*>";
pub const ATTR_RE: &str = r#"([a-zA-Z_:.-]+)\s*=\s*("[^"]*"|'[^']*'|[^\s>]+)"#;
/// Lines that only frame a script: comments, CDATA markers, HTML comment markers (after extruct's open PR #258).
pub const FRAMING_RE: &str = r"//|/\*|\*/|<!--|--!?>|<!\[CDATA\[|\]\]>|\s+";
pub const VOID: [&str; 14] =
    ["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr"];
pub const MICRODATA_PROPS: [&str; 4] = ["price", "lowPrice", "priceCurrency", "availability"];
pub const OFFER_SCOPES: [&str; 7] = ["", "Offer", "AggregateOffer", "Product", "IndividualProduct", "ProductModel", "Event"];
/// hasVariant is walked: a ProductGroup's offers are its variants'
pub const NOT_THE_ITEM: [&str; 6] =
    ["isRelatedTo", "isSimilarTo", "isAccessoryOrSparePartFor", "isConsumableFor", "review", "isVariantOf"];
pub const ITEM_TYPES: &str = r"(?i)Product|Event|Book|Ticket|Vehicle|Game|Album|Movie|SoftwareApplication";
/// `_MAIN_OPEN` (a lookahead: fancy-regex).
pub const MAIN_OPEN: &str = r"(?i)<main(?=[\s>/])";

/// `_price(raw, currency)`: a price from JSON-LD or a tag: a number, or a string like "1,299.00", "$85" or
/// "85.00 USD". In a string with several numbers the one after a currency sign wins ("Qty 3 · $45.00" is 45, not
/// 3). The last of "," or "." is the decimal point when 1-2 digits follow it ("1.299,00", "12,5"), or 3 in a
/// three-decimal currency such as KWD; otherwise every separator groups thousands ("1,299", "1.299.000").
/// changedetection.io's heuristic, re-implemented. Anything not finite, negative, or past MAX_PRICE (an integer
/// past a float included) is no price; a bool is no price.
pub fn price(raw: &Value, currency: &str) -> Option<f64> {
    let _ = (raw, currency);
    todo!("watch._price")
}

/// `_availability`: schema.org availability by its last path segment, letters only: true, false, or None.
pub fn availability(raw: &Value) -> Option<bool> {
    let _ = raw;
    todo!("watch._availability")
}

/// `_ld_blocks`: the text of every ld+json script, in one linear pass (comments dropped first). A script with no
/// close tag ends the scan, as it does in a browser.
pub fn ld_blocks(page: &str) -> Vec<String> {
    let _ = page;
    todo!("watch._ld_blocks")
}

/// `_drop_comments`: the page without <!-- … --> comments, in one linear pass, the pieces joined by a space; an
/// unclosed comment hides the rest.
pub fn drop_comments(page: &str) -> String {
    let _ = page;
    todo!("watch._drop_comments")
}

/// `_drop_blocks`: the page without its `names` elements and its comments, in one linear pass. A tag matches only
/// as a whole name (`<nav>` and `<nav class=…>`, never `<nav-drawer>`). An unclosed block hides the rest.
pub fn drop_blocks(page: &str, names: &[&str]) -> String {
    let _ = (page, names);
    todo!("watch._drop_blocks")
}

/// `_ld_values`: every JSON value in one ld+json script, leniently: raw control characters inside strings are
/// allowed (strict=False), an entity-escaped block is unescaped, framing lines (CDATA, comments) at either end are
/// dropped, and several values back to back are all read. A block that still does not parse gives nothing.
pub fn ld_values(block: &str) -> Vec<Value> {
    let _ = block;
    todo!("watch._ld_values")
}

/// `microdata(page)` (`_Microdata`, an HTMLParser): itemprop price / lowPrice / priceCurrency / availability as
/// (property, value), valued by the W3C microdata rules, kept only when the nearest itemscope is an offer or a
/// product. Never fails.
pub fn microdata(page: &str) -> Vec<(String, String)> {
    let _ = page;
    todo!("watch.microdata")
}

/// `_attrs(tag)`: one tag's attributes, names lowercased, values unquoted and entity-decoded.
pub fn attrs(tag: &str) -> Map<String, Value> {
    let _ = tag;
    todo!("watch._attrs")
}

/// `_offers`: every Offer-like object under `node` (anything with a price, lowPrice or availability), to depth
/// 12; a loyalty tier's price, a unit price and another product's offers are skipped.
pub fn offers(node: &Value, out: &mut Vec<Map<String, Value>>, depth: usize) {
    let _ = (node, out, depth);
    todo!("watch._offers")
}

/// `_typed_name`: a Product's or an Event's name (to depth 6).
pub fn typed_name(node: &Value, depth: usize) -> String {
    let _ = (node, depth);
    todo!("watch._typed_name")
}

/// `_any_name`: the first name of anything not an offer or an organisation (to depth 4).
pub fn any_name(node: &Value, depth: usize) -> String {
    let _ = (node, depth);
    todo!("watch._any_name")
}

/// The price and availability a page states about itself, with no model: JSON-LD offers first (the lowest price
/// among them: a ticket page lists many), then product/og price meta tags, then itemprop price.
pub fn structured(page: &str) -> Reading {
    let _ = page;
    todo!("watch.structured")
}

/// The words a person sees: no scripts, styles or tags, entities decoded, whitespace collapsed.
pub fn visible_text(page: &str) -> String {
    let _ = page;
    todo!("watch.visible_text")
}

/// The visible text of the item, for a model: the page's <main> when it has one, else the page, without the
/// site's nav, footer and aside. A <header> is kept.
pub fn main_text(page: &str) -> String {
    let _ = page;
    todo!("watch.main_text")
}

/// A phrase, case and spacing aside.
pub fn phrase_on(text: &str, phrase: &str) -> bool {
    let _ = (text, phrase);
    todo!("watch.phrase_on")
}
