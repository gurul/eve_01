//! Port of bridge/tests/test_find_image.py: a real, photo-sized picture from Wikipedia for a card, or a plain no.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use eve_find_image::{handle, pick, tools, FetchError, Http, API, OFF_REASON};
use serde_json::{json, Value};

fn page(src: &str, w: i64, h: i64, title: &str, index: i64) -> Value {
    json!({"title": title, "index": index, "original": {"source": src, "width": w, "height": h}})
}

fn arena(src: &str, w: i64, h: i64) -> Value {
    page(src, w, h, "Climate Pledge Arena", 1)
}

struct Down;

#[async_trait]
impl Http for Down {
    async fn get_json(&self, _url: &str, _params: &[(&str, String)]) -> Result<Value, FetchError> {
        Err(FetchError::new("TimeoutError"))
    }
}

type Asked = Vec<(String, Vec<(String, String)>)>;

/// Answers `{}` and remembers what it was asked.
#[derive(Default)]
struct Empty {
    asked: Mutex<Asked>,
}

#[async_trait]
impl Http for Empty {
    async fn get_json(&self, url: &str, params: &[(&str, String)]) -> Result<Value, FetchError> {
        let p = params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        self.asked.lock().unwrap().push((url.to_string(), p));
        Ok(json!({}))
    }
}

struct Canned(Value);

#[async_trait]
impl Http for Canned {
    async fn get_json(&self, _url: &str, _params: &[(&str, String)]) -> Result<Value, FetchError> {
        Ok(self.0.clone())
    }
}

#[test]
fn test_a_photo_sized_picture_is_kept_with_its_page() {
    let got = pick(&json!({"query": {"pages": [arena("https://upload.wikimedia.org/a/Arena.jpg", 2400, 1600)]}}));
    assert_eq!(
        got,
        Some(json!({"ok": true, "image": "https://upload.wikimedia.org/a/Arena.jpg", "width": 2400, "height": 1600,
                    "page": "Climate Pledge Arena", "page_url": "https://en.wikipedia.org/wiki/Climate_Pledge_Arena"}))
    );
}

#[test]
fn test_a_drawn_small_or_plain_http_picture_is_refused() {
    for (src, w, h) in [
        ("https://upload.wikimedia.org/a/Logo.svg", 3000, 3000), // drawn
        ("https://upload.wikimedia.org/a/tiny.jpg", 320, 240),   // small
        ("http://upload.wikimedia.org/a/Arena.jpg", 2400, 1600), // not https
    ] {
        assert_eq!(pick(&json!({"query": {"pages": [arena(src, w, h)]}})), None, "{src}");
    }
}

#[test]
fn test_the_best_ranked_good_page_wins() {
    let pages = json!([page("https://u.org/b.jpg", 1600, 900, "Second", 2),
                       page("https://u.org/a.svg", 900, 900, "First", 1)]);
    // the first has only a drawing
    assert_eq!(pick(&json!({"query": {"pages": pages}})).unwrap()["page"], "Second");
}

#[test]
fn test_the_lower_index_wins_among_good_pages() {
    let pages = json!([page("https://u.org/b.jpg", 1600, 900, "Second", 2),
                       page("https://u.org/a.png?x=1", 1600, 900, "First", 1)]);
    assert_eq!(pick(&json!({"query": {"pages": pages}})).unwrap()["page"], "First");
}

#[test]
fn test_an_empty_or_odd_payload_has_no_picture() {
    for payload in [json!(null), json!({}), json!({"query": null}), json!({"query": {"pages": []}}),
                    json!({"query": {"pages": [{"title": "No picture"}]}})] {
        assert_eq!(pick(&payload), None, "{payload}");
    }
}

#[tokio::test]
async fn test_handle_says_no_plainly_and_never_raises() {
    let empty_env = HashMap::new();
    let got = handle("find_image", &json!({"subject": "Pike Place Market"}), Some(&empty_env), Some(&Down)).await;
    assert_eq!(got["ok"], false);
    assert!(got["reason"].as_str().unwrap().contains("leave the picture out"));
    let empty = Empty::default();
    assert_eq!(handle("find_image", &json!({"subject": "x"}), Some(&empty_env), Some(&empty)).await["ok"], false);
    let off_env = HashMap::from([("EVE_FIND_IMAGE".to_string(), "0".to_string())]);
    let off = handle("find_image", &json!({"subject": "Pike Place"}), Some(&off_env), None).await;
    assert_eq!(off, json!({"ok": false, "reason": OFF_REASON}));
    assert_eq!(tools(Some(&off_env)), Vec::<Value>::new());
}

#[tokio::test]
async fn test_handle_asks_wikipedia_and_answers_with_the_picture() {
    let env = HashMap::new();
    let empty = Empty::default();
    let none = handle("find_image", &json!({"subject": "  Space   Needle "}), Some(&env), Some(&empty)).await;
    assert_eq!(none, json!({"ok": false, "reason": "no high-quality picture of that; leave the picture out"}));
    let asked = empty.asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].0, API);
    assert!(asked[0].1.contains(&("gsrsearch".to_string(), "Space Needle".to_string())));

    let canned = Canned(json!({"query": {"pages": [page("https://upload.wikimedia.org/n/Needle.JPG", 1200, 1800,
                                                         "Space Needle", 1)]}}));
    let got = handle("find_image", &json!({"subject": "Space Needle"}), Some(&env), Some(&canned)).await;
    assert_eq!(got["ok"], true);
    assert_eq!(got["page_url"], "https://en.wikipedia.org/wiki/Space_Needle");

    let other = handle("web_search", &json!({"subject": "Space Needle"}), Some(&env), Some(&canned)).await;
    assert_eq!(other, json!({"ok": false, "reason": "web_search is not the picture tool"}));
    let blank = handle("find_image", &json!({"subject": "   "}), Some(&env), Some(&canned)).await;
    assert_eq!(blank, json!({"ok": false, "reason": "subject must name what to picture"}));
}

#[tokio::test]
#[ignore = "live: needs the network and Wikipedia"]
async fn test_live_wikipedia_gives_a_landmark_its_picture() {
    let env = HashMap::new();
    let got = handle("find_image", &json!({"subject": "Space Needle"}), Some(&env), None).await;
    assert_eq!(got["ok"], true, "{got}");
    assert!(got["image"].as_str().unwrap().starts_with("https://upload.wikimedia.org/"));
    assert!(got["width"].as_i64().unwrap() >= 800);
}

#[test]
fn test_the_brain_is_offered_the_tool_when_on() {
    let names: Vec<Value> = tools(Some(&HashMap::new())).iter().map(|t| t["name"].clone()).collect();
    assert_eq!(names, vec![json!("find_image")]);
}

#[test]
fn test_the_tool_schema_is_pythons() {
    let t = &tools(Some(&HashMap::new()))[0];
    let keys: Vec<&String> = t.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["type", "name", "description", "parameters"]);
    assert_eq!(t["parameters"]["required"], json!(["subject"]));
    assert_eq!(t["parameters"]["additionalProperties"], false);
    assert!(t["description"].as_str().unwrap().ends_with("already carry their own image."));
    assert_eq!(
        t["parameters"]["properties"]["subject"]["description"],
        "What to picture, by its proper name (\"Climate Pledge Arena\", \"Pike Place Market\", \"Phoebe Bridgers\")."
    );
}
