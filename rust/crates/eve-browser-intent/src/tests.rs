//! Ports of bridge/tests/test_browser_intent.py, test_browser_intent_gaps.py and the browser_intent parts of
//! test_site_names.py. The model is a fake here; the real-model tests are `#[ignore = "live: ..."]`.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn no_env() -> HashMap<String, String> {
    HashMap::new()
}

/// Answers every question with a fixed probability table, the way laya-mlx shapes a choice answer.
struct FakeEngine {
    probabilities: Vec<(&'static str, f64)>,
    asked: Mutex<Vec<(String, Value)>>,
}

impl FakeEngine {
    fn new(probabilities: &[(&'static str, f64)]) -> Self {
        FakeEngine { probabilities: probabilities.to_vec(), asked: Mutex::new(Vec::new()) }
    }

    fn asked(&self) -> Vec<(String, Value)> {
        self.asked.lock().clone()
    }
}

impl IntentAsker for FakeEngine {
    fn ask(&self, goal: &str, question: &Value, _state: &Value) -> anyhow::Result<Value> {
        self.asked.lock().push((goal.to_string(), question.clone()));
        let options: Vec<String> = question["criteria"].as_object().unwrap().keys().cloned().collect();
        let given = |k: &str| self.probabilities.iter().find(|(key, _)| *key == k).map(|(_, p)| *p);
        let mut probabilities: Vec<(String, f64)> =
            options.iter().map(|k| (k.clone(), given(k).unwrap_or(0.0))).collect();
        let rest = 1.0 - probabilities.iter().map(|(_, p)| p).sum::<f64>();
        if rest > 0.0 {
            let spare: Vec<String> = options.iter().filter(|k| given(k).is_none()).cloned().collect();
            for (k, p) in probabilities.iter_mut() {
                if spare.contains(k) {
                    *p = rest / spare.len() as f64;
                }
            }
        }
        let mut choice = probabilities[0].clone();
        for (k, p) in &probabilities {
            if *p > choice.1 {
                choice = (k.clone(), *p);
            }
        }
        let probs: Map<String, Value> = probabilities.into_iter().map(|(k, p)| (k, json!(p))).collect();
        Ok(json!({"type": "choice", "choice": choice.0, "probabilities": probs, "confidence": 0.9}))
    }
}

fn pick(kind: &str) -> FakeEngine {
    pick_p(kind, 0.9)
}

fn pick_p(kind: &str, p: f64) -> FakeEngine {
    let key: &'static str = match kind {
        "none" => "unsupported",
        "search" => "search",
        "open_site" => "open_site",
        "open_result" => "open_result",
        other => panic!("unknown kind {other}"),
    };
    FakeEngine::new(&[(key, p)])
}

fn outcome(request: &str, engine: &dyn IntentAsker) -> Option<RequestOutcome> {
    expected_outcome(request, Some(engine), Some(&no_env()))
}

fn keys(options: &[(&str, &str)]) -> HashSet<String> {
    options.iter().map(|(k, _)| k.to_string()).collect()
}

fn set(items: &[&str]) -> HashSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn host(a: &Arguments) -> Option<&'static str> {
    a.site.map(|s| s.host)
}

// ==== test_browser_intent.py ==================================================================================

// ---- literal arguments, all code ----

#[test]
fn test_literal_arguments_come_from_the_words() {
    let a = literal_arguments("search you tube for lofi beats");
    assert_eq!((host(&a), a.query.as_deref(), a.n), (Some("youtube.com"), Some("lofi beats"), None));
    let a = literal_arguments("search for alan turing on wiki pedia and open the first result");
    assert_eq!((host(&a), a.query.as_deref(), a.n), (Some("wikipedia.org"), Some("alan turing"), Some(1)));
    let a = literal_arguments("open git hub");
    assert_eq!((host(&a), a.query.as_deref(), a.n), (Some("github.com"), None, None));
    let a = literal_arguments("open the 3rd result");
    assert_eq!((host(&a), a.query.as_deref(), a.n), (None, None, Some(3)));
    let a = literal_arguments("play the second video");
    assert_eq!(a.n, Some(2));
    let a = literal_arguments("Search for \"rust async\".");
    assert_eq!(a.query.as_deref(), Some("rust async"));
}

// ---- Laya picks the kind ----

#[test]
fn test_search_carries_the_literal_query_and_site() {
    let r = outcome("search you tube for lofi beats", &pick("search")).unwrap();
    assert_eq!(r, RequestOutcome::new(Kind::Search, Some("youtube.com"), Some("lofi beats"), None, 0.9, "laya"));
    assert!(r.latency_ms >= 0.0);
}

#[test]
fn test_open_site_and_open_result() {
    let r = outcome("open git hub", &pick("open_site")).unwrap();
    assert_eq!(
        (r.kind, r.site_host.as_deref(), r.query.as_deref(), r.n, r.source),
        (Kind::OpenSite, Some("github.com"), None, None, "laya")
    );
    let r = outcome("search for alan turing on wikipedia and open the first result", &pick("open_result")).unwrap();
    assert_eq!(
        (r.kind, r.site_host.as_deref(), r.query.as_deref(), r.n),
        (Kind::OpenResult, Some("wikipedia.org"), Some("alan turing"), Some(1))
    );
}

#[test]
fn test_none_is_an_answer_not_an_abstention() {
    let r = outcome("search for cats and email them to my boss", &pick("none")).unwrap();
    assert_eq!((r.kind, r.source), (Kind::None, "laya"));
}

#[test]
fn test_laya_is_asked_in_its_native_shape() {
    let engine = pick("search");
    outcome("search for lofi beats", &engine);
    let (goal, question) = engine.asked().remove(0);
    assert_eq!(goal, "search for lofi beats");
    assert_eq!(question["type"], "choice");
    assert_eq!(question["instructions"]["goal"], "search for lofi beats");
    assert_eq!(question["instructions"]["rules"], RULES);
    let criteria: HashSet<String> = question["criteria"].as_object().unwrap().keys().cloned().collect();
    assert_eq!(criteria, set(&["search", "open_result", "unsupported"]));
}

#[test]
fn test_only_kinds_the_arguments_can_carry_are_offered() {
    let k = |request: &str| keys(&offered(&literal_arguments(request)));
    assert_eq!(k("open git hub"), set(&["open_site", "unsupported"]));
    assert_eq!(k("search you tube for cats"), set(&["search", "open_result", "unsupported"]));
    assert_eq!(k("search you tube for cats and play the first video"), set(&["open_result", "unsupported"]));
    assert_eq!(k("open the second result"), set(&["open_result", "unsupported"]));
}

#[test]
fn test_offered_keeps_laya_browse_order_and_text() {
    let options = offered(&literal_arguments("search you tube for cats"));
    assert_eq!(
        options,
        vec![
            ("search", "Search for the requested topic and show the results."),
            ("open_result", "Open a particular video, article, repository or item from search results."),
            ("unsupported", "Not a command, ambiguous, or these outcomes omit part of the request"),
        ]
    );
}

#[test]
fn test_no_arguments_at_all_is_none_from_code_without_asking_laya() {
    let engine = pick("search");
    let r = outcome("turn up the volume", &engine).unwrap();
    assert_eq!((r.kind, r.source, r.confidence), (Kind::None, "code", 1.0));
    assert!(engine.asked().is_empty());
}

// ---- abstentions ----

#[test]
fn test_a_kind_the_arguments_cannot_carry_abstains() {
    for (request, kind) in [
        ("open git hub", "search"),                // search with no query
        ("search for lofi beats", "open_site"),    // open a site that was never named
        ("search for lofi beats", "open_result"),  // open a result with no number
        ("search you tube for cats", "open_site"), // opening the site drops the spoken query
    ] {
        assert!(outcome(request, &pick(kind)).is_none(), "{request} / {kind}");
    }
}

#[test]
fn test_the_gate_abstains_on_low_confidence_or_a_thin_margin() {
    assert!(outcome("search for lofi beats", &FakeEngine::new(&[("search", 0.45), ("open_result", 0.2)])).is_none());
    assert!(outcome("search for lofi beats", &FakeEngine::new(&[("search", 0.52), ("open_result", 0.46)])).is_none());
    assert!(outcome("search for lofi beats", &FakeEngine::new(&[("search", 0.55), ("open_result", 0.40)])).is_some());
}

#[test]
fn test_a_malformed_answer_abstains() {
    struct Broken;
    impl IntentAsker for Broken {
        fn ask(&self, _: &str, _: &Value, _: &Value) -> anyhow::Result<Value> {
            Ok(json!({"choice": "search", "probabilities": {"search": 2.0}}))
        }
    }
    assert!(expected_outcome("search for lofi beats", Some(&Broken), Some(&no_env())).is_none());

    struct Raises;
    impl IntentAsker for Raises {
        fn ask(&self, _: &str, _: &Value, _: &Value) -> anyhow::Result<Value> {
            anyhow::bail!("metal went away")
        }
    }
    assert!(expected_outcome("search for lofi beats", Some(&Raises), Some(&no_env())).is_none());
}

#[test]
fn test_validate_choice_schema_checks() {
    let options = offered(&literal_arguments("search for lofi beats"));
    let ok = json!({"choice": "search", "probabilities": {"search": 0.8, "open_result": 0.1, "unsupported": 0.1}});
    assert_eq!(validate_choice(&ok, &options), Ok(("search".to_string(), 0.8)));
    // a bool is not a probability (Python's `type(v) in {int, float}`)
    let b = json!({"choice": "search", "probabilities": {"search": true, "open_result": 0, "unsupported": 0}});
    assert_eq!(validate_choice(&b, &options), Err(Abstain("malformed answer".into())));
    // an int is
    let i = json!({"choice": "search", "probabilities": {"search": 1, "open_result": 0, "unsupported": 0}});
    assert_eq!(validate_choice(&i, &options), Ok(("search".to_string(), 1.0)));
    // a choice that is not the top
    let not_top = json!({"choice": "open_result", "probabilities": {"search": 0.8, "open_result": 0.1, "unsupported": 0.1}});
    assert!(validate_choice(&not_top, &options).is_err());
    // keys that are not the options
    let extra = json!({"choice": "search", "probabilities": {"search": 0.8, "open_site": 0.2}});
    assert!(validate_choice(&extra, &options).is_err());
    // not summing to one
    let sum = json!({"choice": "search", "probabilities": {"search": 0.8, "open_result": 0.1, "unsupported": 0.05}});
    assert!(validate_choice(&sum, &options).is_err());
    assert!(validate_choice(&json!([1, 2]), &options).is_err());
    let thin = json!({"choice": "search", "probabilities": {"search": 0.5, "open_result": 0.45, "unsupported": 0.05}});
    assert_eq!(validate_choice(&thin, &options), Err(Abstain("under the gate".into())));
}

#[test]
fn test_off_switch() {
    let engine = pick("search");
    let off: HashMap<String, String> = [("EVE_BROWSER_INTENT".to_string(), "off".to_string())].into();
    assert!(expected_outcome("search for cats", Some(&engine), Some(&off)).is_none());
    assert!(engine.asked().is_empty());
    let env = |v: &str| -> HashMap<String, String> { [("EVE_BROWSER_INTENT".to_string(), v.to_string())].into() };
    assert_eq!(mode(Some(&no_env())), "laya");
    assert_eq!(mode(Some(&env("LAYA"))), "laya");
    assert_eq!(mode(Some(&env(" laya "))), "laya");
    assert_eq!(mode(Some(&env(""))), "laya");
    assert_eq!(mode(Some(&env("jev"))), "off");
}

// ---- the real engine: lazy, warm once, abstains cleanly ----

/// A runtime whose agent answers "search" at 0.94, the way the Python tests' fake laya_mlx does.
#[derive(Clone, Default)]
struct FakeRuntime {
    loads: Arc<AtomicUsize>,
    released: Arc<AtomicUsize>,
    fail: Option<LoadError>,
}

struct FakeAgent {
    released: Arc<AtomicUsize>,
}

impl LayaAgent for FakeAgent {
    fn prefix_fits(&mut self, _question: &Value) -> anyhow::Result<bool> {
        Ok(true)
    }

    fn predict(&mut self, _state: &Value, questions: &Value) -> anyhow::Result<Value> {
        let (qid, q) = questions.as_object().unwrap().iter().next().unwrap();
        let criteria = q["criteria"].as_object().unwrap();
        let probs: Map<String, Value> = criteria
            .keys()
            .map(|k| (k.clone(), json!(if k == "search" { 0.94 } else { 0.06 / (criteria.len() - 1) as f64 })))
            .collect();
        Ok(json!({"answers": {qid.clone(): {"type": "choice", "choice": "search", "probabilities": probs}}}))
    }

    fn release(self: Box<Self>) {
        self.released.fetch_add(1, Ordering::SeqCst);
    }
}

impl LayaRuntime for FakeRuntime {
    fn load(&self) -> Result<Box<dyn LayaAgent>, LoadError> {
        self.loads.fetch_add(1, Ordering::SeqCst);
        match &self.fail {
            Some(e) => Err(e.clone()),
            None => Ok(Box::new(FakeAgent { released: self.released.clone() })),
        }
    }
}

fn engine_with(runtime: &FakeRuntime, idle_s: Option<f64>, clock: Option<Clock>, timer: bool) -> LayaKindEngine {
    LayaKindEngine::new(EngineOptions { idle_s, clock, timer, runtime: Some(Box::new(runtime.clone())) })
}

#[test]
fn test_abstains_cleanly_when_laya_mlx_is_absent() {
    let runtime = FakeRuntime { fail: Some(LoadError::Unavailable("No module named 'laya_mlx'".into())), ..Default::default() };
    let engine = engine_with(&runtime, None, None, true);
    assert!(expected_outcome("search you tube for cats", Some(&engine), Some(&no_env())).is_none());
    assert!(!engine.loaded());
    assert_eq!(engine.warm(), Err(Abstain("laya-mlx unavailable: No module named 'laya_mlx'".into())));
}

#[test]
fn test_a_failed_load_abstains_and_is_not_retried_every_call() {
    let runtime = FakeRuntime { fail: Some(LoadError::LoadFailed("weights missing".into())), ..Default::default() };
    let engine = engine_with(&runtime, None, None, true);
    for _ in 0..3 {
        assert!(expected_outcome("search you tube for cats", Some(&engine), Some(&no_env())).is_none());
    }
    assert_eq!(runtime.loads.load(Ordering::SeqCst), 1);
    assert_eq!(engine.warm(), Err(Abstain("load failed: weights missing".into())));
}

#[test]
fn test_not_cached_abstains_without_the_network() {
    let runtime = FakeRuntime { fail: Some(LoadError::NotCached), ..Default::default() };
    let engine = engine_with(&runtime, None, None, true);
    assert!(expected_outcome("search for cats", Some(&engine), Some(&no_env())).is_none());
    assert_eq!(engine.warm(), Err(Abstain("cklxx/laya-browser@8a7e625 is not cached".into())));
}

#[test]
fn test_warm_loads_once_under_concurrency() {
    let runtime = FakeRuntime::default();
    let engine = Arc::new(engine_with(&runtime, None, None, true));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let engine = engine.clone();
            std::thread::spawn(move || expected_outcome("search for lofi beats", Some(&*engine), Some(&no_env())))
        })
        .collect();
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(runtime.loads.load(Ordering::SeqCst), 1);
    assert!(results.iter().all(|r| r.as_ref().is_some_and(|r| r.kind == Kind::Search)));
    assert!(recent_latencies().len() >= 8);
    engine.unload();
}

// ---- the real checkpoint, when it is already on this Mac ----

fn live_engine() -> LayaKindEngine {
    LayaKindEngine::new(EngineOptions { timer: false, ..Default::default() })
}

#[test]
#[ignore = "live: needs laya-mlx (EVE_BROWSER_INTENT_PYTHON) and the cklxx/laya-browser checkpoint cached"]
fn test_the_real_checkpoint_on_three_obvious_requests() {
    let engine = live_engine();
    for (request, kind) in [
        ("search YouTube for lofi beats", Kind::Search),
        ("open git hub", Kind::OpenSite),
        ("search wikipedia for alan turing and open the first result", Kind::OpenResult),
    ] {
        let r = expected_outcome(request, Some(&engine), Some(&no_env()));
        assert!(r.as_ref().is_some_and(|r| r.kind == kind), "{request}: {r:?}");
        let r = r.unwrap();
        assert!(r.source == "laya" && r.latency_ms > 0.0);
    }
    engine.unload();
}

// ==== test_browser_intent_gaps.py =============================================================================

// ---- the site table ----

#[test]
fn test_common_sites_are_in_the_table() {
    for (said, host) in [
        ("npm", "npmjs.com"), ("Spotify", "spotify.com"), ("IKEA", "ikea.com"), ("Bing", "bing.com"),
        ("BBC", "bbc.co.uk"), ("IMDb", "imdb.com"), ("Twitter", "x.com"), ("Facebook", "facebook.com"),
        ("Instagram", "instagram.com"), ("TikTok", "tiktok.com"), ("Pinterest", "pinterest.com"),
        ("Twitch", "twitch.tv"), ("Yahoo", "yahoo.com"), ("Etsy", "etsy.com"), ("Walmart", "walmart.com"),
        ("Target", "target.com"), ("Best Buy", "bestbuy.com"), ("Craigslist", "craigslist.org"),
        ("Zillow", "zillow.com"), ("Yelp", "yelp.com"), ("Tripadvisor", "tripadvisor.com"),
        ("Airbnb", "airbnb.com"), ("Booking.com", "booking.com"), ("Expedia", "expedia.com"),
        ("Quora", "quora.com"), ("Medium", "medium.com"), ("PyPI", "pypi.org"), ("Hugging Face", "huggingface.co"),
        ("arXiv", "arxiv.org"), ("CNN", "cnn.com"), ("New York Times", "nytimes.com"), ("Apple", "apple.com"),
        ("Steam", "steampowered.com"), ("SoundCloud", "soundcloud.com"), ("Vimeo", "vimeo.com"),
        ("Goodreads", "goodreads.com"), ("Wolfram Alpha", "wolframalpha.com"), ("Indeed", "indeed.com"),
        ("Glassdoor", "glassdoor.com"), ("Home Depot", "homedepot.com"), ("Costco", "costco.com"),
        ("ESPN", "espn.com"), ("Gmail", "mail.google.com"), ("Google Maps", "maps.google.com"),
    ] {
        let site = site_for(said);
        assert!(site.is_some_and(|s| s.host == host), "{said}");
        assert!(site.unwrap().home.starts_with("https://"));
    }
}

#[test]
fn test_the_table_is_broad_and_consistent() {
    assert!(SITES.len() >= 40);
    let hosts: HashSet<&str> = SITES.iter().map(|s| s.host).collect();
    assert_eq!(hosts.len(), SITES.len());
    let names: HashSet<String> = SITES.iter().map(|s| casefold(s.name)).collect();
    assert_eq!(names.len(), SITES.len());
    for s in SITES {
        if s.search_template.is_empty() {
            assert!(s.query_key.is_empty(), "{}", s.name);
            continue;
        }
        let address = s.search_url("blue shoes & socks");
        let parts = url::Url::parse(&address).unwrap();
        assert!(parts.scheme() == "https" && parts.host_str().is_some(), "{}", s.name);
        if !s.query_key.is_empty() {
            assert!(parts.query_pairs().any(|(_, v)| v == "blue shoes & socks"), "{} {address}", s.name);
        } else {
            // a path search: the query is encoded into the path, spaces as %20, never a bare "&"
            assert!(parts.path().contains("blue%20shoes%20%26%20socks"), "{} {address}", s.name);
        }
    }
}

#[test]
fn test_short_or_shared_host_labels_are_not_names() {
    assert!(site_for("x").is_none()); // x.com is Twitter's host, but "x" names nothing
    assert!(site_for("mail").is_none()); // mail.google.com
    assert!(site_for("news").is_none()); // news.ycombinator.com
    assert_eq!(site_for("google").unwrap().host, "google.com");
}

#[test]
fn test_new_sites_are_found_in_requests() {
    for (said, host) in [
        ("pull up imdb", "imdb.com"),
        ("search spotify for jazz piano", "spotify.com"),
        ("look for bookshelves on ikea", "ikea.com"),
        ("open the bbc", "bbc.co.uk"),
        ("search n p m for left pad", "npmjs.com"),
        ("go to target", "target.com"),
        ("is it in stock at target", "target.com"),
        ("open medium.com", "medium.com"),
        ("browse air bnb for cabins", "airbnb.com"),
        ("visit sound cloud", "soundcloud.com"),
        ("search face book for my old school", "facebook.com"),
    ] {
        assert!(mentioned_site(said).is_some_and(|s| s.host == host), "{said}");
    }
}

#[test]
fn test_everyday_words_are_not_sites() {
    for said in [
        "what's my target heart rate",
        "cook the steak on medium heat",
        "find me an apple pie recipe",
        "let off some steam",
        "indeed it is",
        "I need to make a booking for dinner",
        "that was the best buy I made all year",
        "he gave me a twitch of a smile",
    ] {
        assert!(mentioned_site(said).is_none(), "{said}");
    }
}

// ---- queries ----

#[test]
fn test_query_phrase_families() {
    for (said, query) in [
        ("search the web for cheap flights to lisbon", "cheap flights to lisbon"),
        ("search the internet for vegan lasagna", "vegan lasagna"),
        ("search online for standing desks", "standing desks"),
        ("look online for a used bike", "used bike"),
        ("look on the internet for jazz clubs in berlin", "jazz clubs in berlin"),
        ("web search mechanical keyboards", "mechanical keyboards"),
        ("do a web search for solar panel prices", "solar panel prices"),
        ("can you do a quick search for tide times", "tide times"),
        ("google best ramen in tokyo", "best ramen in tokyo"),
        ("search youtube lofi beats", "lofi beats"),
        ("search for weather in paris on bing", "weather in paris"),
    ] {
        assert_eq!(literal_arguments(said).query.as_deref(), Some(query), "{said}");
    }
}

#[test]
fn test_filler_is_trimmed_but_the_query_kept() {
    for (said, query) in [
        ("please search for hiking boots please", "hiking boots"),
        ("search for the history of rome", "history of rome"),
        ("look up ramen near me for me", "ramen near me"),
        ("search for pasta recipes and tell me the best one", "pasta recipes"),
        ("can you look up the opening hours of the louvre real quick", "opening hours of the louvre"),
        ("search for flights to oslo so I can plan the trip", "flights to oslo"),
        ("um search for uh noise cancelling headphones", "noise cancelling headphones"),
        ("search for cats and dogs", "cats and dogs"),
    ] {
        assert_eq!(literal_arguments(said).query.as_deref(), Some(query), "{said}");
    }
}

#[test]
fn test_search_results_words_are_not_a_query() {
    let a = literal_arguments("open the second search result");
    assert_eq!((a.query.as_deref(), a.n), (None, Some(2)));
    assert!(literal_arguments("click the search bar").query.is_none());
}

// ---- result numbers with no noun ----

#[test]
fn test_result_numbers_without_a_noun() {
    for (said, n) in [
        ("open number two", 2),
        ("click number 3", 3),
        ("play the sixth", 6),
        ("go with the fourth please", 4),
        ("pick the second and read it to me", 2),
        ("the seventh", 7),
        ("number five", 5),
        ("tap on the third", 3),
    ] {
        assert_eq!(literal_arguments(said).n, Some(n), "{said}");
    }
}

#[test]
fn test_an_ordinal_that_is_not_a_result_number() {
    for said in [
        "search for the first moon landing",
        "the second half of the game was great",
        "what's the number two song this week",
        "remind me on the third",
        "who was the sixth president",
    ] {
        assert_eq!(literal_arguments(said).n, None, "{said}");
    }
}

#[test]
fn test_an_ordinal_inside_a_query_keeps_the_search() {
    let a = literal_arguments("search for the first moon landing");
    assert_eq!((a.query.as_deref(), a.n), (Some("first moon landing"), None));
    assert_eq!(keys(&offered(&a)), set(&["search", "open_result", "unsupported"]));
}

#[test]
fn test_result_number_zero_and_numerals() {
    assert_eq!(literal_arguments("open result 0").n, None);
    assert_eq!(literal_arguments("open the 12th result").n, Some(12));
    // a non-ASCII decimal digit is a digit, as Python's \d and int() read it
    assert_eq!(literal_arguments("open result ٣").n, Some(3));
}

// ---- how Laya is asked ----

struct Recorder {
    key: &'static str,
    goals: Mutex<Vec<String>>,
}

impl Recorder {
    fn new(key: &'static str) -> Self {
        Recorder { key, goals: Mutex::new(Vec::new()) }
    }
}

impl IntentAsker for Recorder {
    fn ask(&self, goal: &str, question: &Value, _state: &Value) -> anyhow::Result<Value> {
        self.goals.lock().push(goal.to_string());
        assert_eq!(question["instructions"]["goal"], goal);
        let options: Vec<String> = question["criteria"].as_object().unwrap().keys().cloned().collect();
        let probs: Map<String, Value> = options
            .iter()
            .map(|k| (k.clone(), json!(if k == self.key { 0.9 } else { 0.1 / (options.len() - 1) as f64 })))
            .collect();
        Ok(json!({"type": "choice", "choice": self.key, "probabilities": probs}))
    }
}

#[test]
fn test_navigation_verbs_reach_laya_as_open() {
    for (said, goal) in [
        ("um go to sound cloud", "open SoundCloud"),
        ("pull up yelp", "open yelp"),
        ("take me to git hub", "open GitHub"),
        ("navigate to the bbc", "open the bbc"),
        ("bring up reddit please", "open reddit please"),
        ("click on the fifth video", "open the fifth video"),
        ("uh go into the second one", "open the second one"),
        ("tap number three", "open number three"),
        ("play the second", "open the second"),
        ("watch the third one please", "open the third one please"),
        ("the sixth", "open the sixth"),
        ("number two", "open number two"),
    ] {
        let rec = Recorder::new("unsupported");
        expected_outcome(said, Some(&rec), Some(&no_env()));
        assert_eq!(*rec.goals.lock(), vec![goal.to_string()], "{said}");
    }
}

#[test]
fn test_a_second_step_reaches_laya_as_said() {
    // Laya reads "open X and ..." as plain open_site, so a request with a second step keeps its own verb.
    for said in [
        "pull up netflix and log into my account",
        "take me to GitHub and star laya",
        "click on the second link, then copy it",
        "go to youtube settings then turn off autoplay",
    ] {
        let rec = Recorder::new("unsupported");
        expected_outcome(said, Some(&rec), Some(&no_env()));
        assert_eq!(*rec.goals.lock(), vec![join_site_names(said)], "{said}");
    }
}

#[test]
fn test_a_second_step_is_not_part_of_the_query() {
    assert_eq!(literal_arguments("search youtube for cats and log in").query.as_deref(), Some("cats"));
    assert_eq!(
        literal_arguments("search reddit for keyboards and subscribe to the top one").query.as_deref(),
        Some("keyboards")
    );
}

#[test]
fn test_other_verbs_reach_laya_unchanged() {
    let rec = Recorder::new("search");
    let r = expected_outcome("search the web for cheap flights", Some(&rec), Some(&no_env()));
    assert_eq!(*rec.goals.lock(), vec!["search the web for cheap flights".to_string()]);
    let r = r.unwrap();
    assert_eq!((r.kind, r.query.as_deref(), r.site_host.as_deref()), (Kind::Search, Some("cheap flights"), None));
}

#[test]
fn test_a_bare_result_number_offers_only_open_result() {
    let a = literal_arguments("open number two");
    assert_eq!(keys(&offered(&a)), set(&["open_result", "unsupported"]));
    let r = expected_outcome("open number two", Some(&Recorder::new("open_result")), Some(&no_env())).unwrap();
    assert_eq!((r.kind, r.n), (Kind::OpenResult, Some(2)));
}

// ---- Laya is unloaded after an idle period and reloaded lazily ----

fn manual_clock(start: f64) -> (Arc<Mutex<f64>>, Clock) {
    let now = Arc::new(Mutex::new(start));
    let reader = now.clone();
    (now, Arc::new(move || *reader.lock()))
}

#[test]
fn test_idle_seconds_is_env_tunable() {
    let env = |v: &str| -> HashMap<String, String> { [("EVE_BROWSER_INTENT_IDLE_S".to_string(), v.to_string())].into() };
    assert_eq!(idle_seconds(Some(&no_env())), 600.0);
    assert_eq!(idle_seconds(Some(&env("30"))), 30.0);
    assert_eq!(idle_seconds(Some(&env("0"))), 0.0); // 0 keeps Laya loaded
    assert_eq!(idle_seconds(Some(&env("soon"))), 600.0);
    assert_eq!(idle_seconds(Some(&env(" 1_200 "))), 1200.0);
    assert_eq!(idle_seconds(Some(&env("-5"))), 600.0);
    assert_eq!(idle_seconds(Some(&env("inf"))), 600.0);
    assert_eq!(idle_seconds(Some(&env("1e2"))), 100.0);
}

#[test]
fn test_unloads_after_idle_and_reloads_lazily() {
    let runtime = FakeRuntime::default();
    let (now, clock) = manual_clock(1000.0);
    let engine = engine_with(&runtime, Some(600.0), Some(clock), false);
    let r = expected_outcome("search for lofi beats", Some(&engine), Some(&no_env())).unwrap();
    assert_eq!(r.kind, Kind::Search);
    assert!(engine.loaded() && runtime.loads.load(Ordering::SeqCst) == 1);
    *now.lock() += 599.0;
    assert!(!engine.unload_if_idle() && engine.loaded());
    *now.lock() += 2.0;
    assert!(engine.unload_if_idle());
    assert!(!engine.loaded() && runtime.released.load(Ordering::SeqCst) >= 1);
    let r = expected_outcome("search for lofi beats", Some(&engine), Some(&no_env())).unwrap();
    assert_eq!(r.kind, Kind::Search);
    assert!(engine.loaded() && runtime.loads.load(Ordering::SeqCst) == 2);
}

#[test]
fn test_use_resets_the_idle_clock() {
    let runtime = FakeRuntime::default();
    let (now, clock) = manual_clock(0.0);
    let engine = engine_with(&runtime, Some(600.0), Some(clock), false);
    expected_outcome("search for lofi beats", Some(&engine), Some(&no_env()));
    *now.lock() = 500.0;
    expected_outcome("search for lofi beats", Some(&engine), Some(&no_env()));
    *now.lock() = 1000.0;
    assert!(!engine.unload_if_idle() && engine.loaded());
    *now.lock() = 1101.0;
    assert!(engine.unload_if_idle());
}

#[test]
fn test_zero_idle_never_unloads() {
    let runtime = FakeRuntime::default();
    let (now, clock) = manual_clock(0.0);
    let engine = engine_with(&runtime, Some(0.0), Some(clock), false);
    expected_outcome("search for lofi beats", Some(&engine), Some(&no_env()));
    *now.lock() = 1e9;
    assert!(!engine.unload_if_idle() && engine.loaded());
}

#[test]
fn test_the_idle_timer_really_unloads() {
    let runtime = FakeRuntime::default();
    let engine = engine_with(&runtime, Some(0.05), None, true);
    assert!(expected_outcome("search for lofi beats", Some(&engine), Some(&no_env())).is_some());
    let deadline = Instant::now() + Duration::from_secs(3);
    while engine.loaded() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!engine.loaded());
    assert!(expected_outcome("search for lofi beats", Some(&engine), Some(&no_env())).is_some());
    assert_eq!(runtime.loads.load(Ordering::SeqCst), 2);
    engine.unload();
}

#[test]
fn test_unload_stops_the_timer_and_drops_the_model() {
    let runtime = FakeRuntime::default();
    let engine = engine_with(&runtime, Some(600.0), None, true);
    engine.warm().unwrap();
    assert!(engine.loaded());
    engine.unload();
    assert!(!engine.loaded());
    assert_eq!(runtime.released.load(Ordering::SeqCst), 1);
}

// ---- the helper process, against fake laya_mlx / mlx / huggingface_hub modules ----

/// A system Python, for the boundary tests; None skips them on a machine without one.
fn python3() -> Option<PathBuf> {
    let ok = Command::new("python3").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status();
    ok.is_ok_and(|s| s.success()).then(|| PathBuf::from("python3"))
}

const FAKE_HUB: &str = r#"
import os
def snapshot_download(repo_id, revision=None, local_files_only=False, allow_patterns=None):
    with open(os.environ["FAKE_RECORD"], "a") as f:
        import json
        f.write(json.dumps({"snapshot": [repo_id, revision, local_files_only, allow_patterns]}) + "\n")
    path = os.environ.get("FAKE_SNAPSHOT", "")
    if not path:
        raise FileNotFoundError("not cached")
    return path
"#;

const FAKE_MLX_CORE: &str = r#"
import json, os
def set_cache_limit(n):
    with open(os.environ["FAKE_RECORD"], "a") as f:
        f.write(json.dumps({"cache": n}) + "\n")
"#;

const FAKE_LAYA: &str = r#"
import json, os
print("library chatter on stdout must not reach the protocol")

class Tok:
    mask_token = "<mask>"
    def __call__(self, s, add_special_tokens=True):
        return {"input_ids": s.split()}

class Agent:
    tok = Tok()
    cfg = {"max_len": 512, "head_max_len": int(os.environ.get("FAKE_HEAD", "192"))}
    def _to_internal(self, q):
        return {"t": q["type"], "ins": q["instructions"]["goal"], "opts": list(q["criteria"].values())}
    def predict(self, state, questions):
        if os.environ.get("FAKE_DIE"):
            os._exit(3)
        (qid, q), = questions.items()
        probs = {k: (0.94 if k == "search" else 0.06 / (len(q["criteria"]) - 1)) for k in q["criteria"]}
        return {"answers": {qid: {"type": "choice", "choice": "search", "probabilities": probs}}}

def load(*args, **kwargs):
    with open(os.environ["FAKE_RECORD"], "a") as f:
        f.write(json.dumps({"load": list(args), "kwargs": kwargs}) + "\n")
    if os.environ.get("FAKE_LOAD_FAILS"):
        raise OSError("weights missing")
    return Agent()
"#;

const FAKE_LAYA_COMMON: &str = r#"
def render_options(q):
    return q["opts"]
"#;

struct FakeModules {
    dir: tempfile::TempDir,
}

impl FakeModules {
    fn new(laya_installed: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        std::fs::create_dir_all(p.join("huggingface_hub")).unwrap();
        std::fs::write(p.join("huggingface_hub/__init__.py"), FAKE_HUB).unwrap();
        std::fs::create_dir_all(p.join("mlx")).unwrap();
        std::fs::write(p.join("mlx/__init__.py"), "").unwrap();
        std::fs::write(p.join("mlx/core.py"), FAKE_MLX_CORE).unwrap();
        std::fs::create_dir_all(p.join("laya_mlx")).unwrap();
        if laya_installed {
            std::fs::write(p.join("laya_mlx/__init__.py"), FAKE_LAYA).unwrap();
            std::fs::write(p.join("laya_mlx/common.py"), FAKE_LAYA_COMMON).unwrap();
        } else {
            std::fs::write(p.join("laya_mlx/__init__.py"), "raise ImportError(\"No module named 'laya_mlx'\")\n")
                .unwrap();
        }
        std::fs::write(p.join("record.jsonl"), "").unwrap();
        FakeModules { dir }
    }

    fn runtime(&self, python: PathBuf, extra: &[(&str, &str)]) -> LayaSubprocess {
        let mut env: Vec<(OsString, OsString)> = vec![
            ("PYTHONPATH".into(), self.dir.path().as_os_str().to_owned()),
            ("FAKE_RECORD".into(), self.dir.path().join("record.jsonl").into_os_string()),
        ];
        env.extend(extra.iter().map(|(k, v)| (OsString::from(k), OsString::from(v))));
        LayaSubprocess { python, env }
    }

    fn record(&self) -> Vec<Value> {
        std::fs::read_to_string(self.dir.path().join("record.jsonl"))
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}

fn subprocess_engine(runtime: LayaSubprocess) -> LayaKindEngine {
    LayaKindEngine::new(EngineOptions { idle_s: Some(600.0), clock: None, timer: false, runtime: Some(Box::new(runtime)) })
}

#[test]
fn test_helper_abstains_cleanly_when_laya_mlx_is_absent() {
    let Some(python) = python3() else { return };
    let fakes = FakeModules::new(false);
    let engine = subprocess_engine(fakes.runtime(python, &[("FAKE_SNAPSHOT", "/cached")]));
    assert!(expected_outcome("search you tube for cats", Some(&engine), Some(&no_env())).is_none());
    assert!(!engine.loaded());
    assert_eq!(engine.warm(), Err(Abstain("laya-mlx unavailable: No module named 'laya_mlx'".into())));
}

#[test]
fn test_helper_failed_load_is_not_retried_every_call() {
    let Some(python) = python3() else { return };
    let fakes = FakeModules::new(true);
    let engine = subprocess_engine(fakes.runtime(python, &[("FAKE_SNAPSHOT", "/nowhere"), ("FAKE_LOAD_FAILS", "1")]));
    for _ in 0..3 {
        assert!(expected_outcome("search you tube for cats", Some(&engine), Some(&no_env())).is_none());
    }
    let record = fakes.record();
    let loads: Vec<&Value> = record.iter().filter(|r| r.get("load").is_some()).collect();
    assert_eq!(loads.len(), 1);
    assert!(record.contains(&json!({"cache": MLX_CACHE_LIMIT_BYTES})));
    assert_eq!(loads[0]["load"], json!(["/nowhere"]));
    assert_eq!(loads[0]["kwargs"]["subfolder"], BROWSER_SUBFOLDER);
    assert_eq!(loads[0]["kwargs"]["revision"], BROWSER_REVISION);
    assert_eq!(loads[0]["kwargs"]["dtype"], "float16");
    // no prompt cache that every request would miss
    assert!(loads[0]["kwargs"].get("cache_prompts").is_none());
    let snapshot = record.iter().find_map(|r| r.get("snapshot")).unwrap();
    assert_eq!(snapshot, &json!([BROWSER_MODEL, BROWSER_REVISION, true, ["v10s/*"]]));
    assert_eq!(engine.warm(), Err(Abstain("load failed: weights missing".into())));
}

#[test]
fn test_helper_not_cached_abstains_without_the_network() {
    let Some(python) = python3() else { return };
    let fakes = FakeModules::new(true);
    let engine = subprocess_engine(fakes.runtime(python, &[]));
    assert!(expected_outcome("search for cats", Some(&engine), Some(&no_env())).is_none());
    assert!(fakes.record().iter().all(|r| r.get("load").is_none()), "must not load (or download) an uncached checkpoint");
    assert_eq!(engine.warm(), Err(Abstain("cklxx/laya-browser@8a7e625 is not cached".into())));
}

#[test]
fn test_helper_answers_and_unloads() {
    let Some(python) = python3() else { return };
    let fakes = FakeModules::new(true);
    let engine = subprocess_engine(fakes.runtime(python, &[("FAKE_SNAPSHOT", "/cached")]));
    for _ in 0..2 {
        let r = expected_outcome("search for lofi beats", Some(&engine), Some(&no_env())).unwrap();
        assert_eq!((r.kind, r.query.as_deref(), r.confidence, r.source), (Kind::Search, Some("lofi beats"), 0.94, "laya"));
    }
    assert!(engine.loaded());
    assert_eq!(fakes.record().iter().filter(|r| r.get("load").is_some()).count(), 1);
    engine.unload();
    assert!(!engine.loaded());
    assert!(expected_outcome("search for lofi beats", Some(&engine), Some(&no_env())).is_some());
    assert_eq!(fakes.record().iter().filter(|r| r.get("load").is_some()).count(), 2);
    engine.unload();
}

#[test]
fn test_helper_refuses_a_question_over_the_head_budget() {
    let Some(python) = python3() else { return };
    let fakes = FakeModules::new(true);
    let engine = subprocess_engine(fakes.runtime(python, &[("FAKE_SNAPSHOT", "/cached"), ("FAKE_HEAD", "10")]));
    assert!(expected_outcome("search for lofi beats", Some(&engine), Some(&no_env())).is_none());
    // a refusal is not a failure: the model stays loaded
    assert!(engine.loaded());
    engine.unload();
}

#[test]
fn test_helper_that_dies_is_reloaded_on_the_next_question() {
    let Some(python) = python3() else { return };
    let fakes = FakeModules::new(true);
    let engine = subprocess_engine(fakes.runtime(python, &[("FAKE_SNAPSHOT", "/cached"), ("FAKE_DIE", "1")]));
    assert!(expected_outcome("search for lofi beats", Some(&engine), Some(&no_env())).is_none());
    assert!(!engine.loaded());
    assert!(expected_outcome("search for lofi beats", Some(&engine), Some(&no_env())).is_none());
    assert_eq!(fakes.record().iter().filter(|r| r.get("load").is_some()).count(), 2);
}

#[test]
fn test_helper_locates_the_cached_checkpoint() {
    let Some(python) = python3() else { return };
    let fakes = FakeModules::new(true);
    assert_eq!(fakes.runtime(python.clone(), &[("FAKE_SNAPSHOT", "/cached")]).cached_checkpoint(), Some(PathBuf::from("/cached")));
    assert_eq!(fakes.runtime(python, &[]).cached_checkpoint(), None);
}

#[test]
fn test_missing_interpreter_is_unavailable() {
    let runtime = LayaSubprocess { python: PathBuf::from("/nonexistent/python3"), env: Vec::new() };
    let engine = subprocess_engine(runtime);
    assert!(expected_outcome("search for cats", Some(&engine), Some(&no_env())).is_none());
    assert!(engine.warm().unwrap_err().0.starts_with("laya-mlx unavailable: "));
}

#[test]
fn test_helper_args() {
    let runtime = LayaSubprocess { python: PathBuf::from("python3"), env: Vec::new() };
    assert_eq!(
        runtime.args("serve"),
        vec!["serve", BROWSER_MODEL, BROWSER_REVISION, "v10s", "67108864"]
    );
    assert_eq!(runtime.args("locate"), vec!["locate", BROWSER_MODEL, BROWSER_REVISION, "v10s"]);
}

// ---- the real checkpoint, on phrasings outside the first eval ----

#[test]
#[ignore = "live: needs laya-mlx (EVE_BROWSER_INTENT_PYTHON) and the cklxx/laya-browser checkpoint cached"]
fn test_the_real_checkpoint_on_the_closed_gaps() {
    let engine = live_engine();
    type Want = (Kind, Option<&'static str>, Option<&'static str>, Option<u32>);
    let expected: [(&str, Want); 7] = [
        ("pull up yelp", (Kind::OpenSite, Some("yelp.com"), None, None)),
        ("take me to sound cloud", (Kind::OpenSite, Some("soundcloud.com"), None, None)),
        ("uh click on the second video", (Kind::OpenResult, None, None, Some(2))),
        ("go with number three", (Kind::OpenResult, None, None, Some(3))),
        ("search the internet for vegan lasagna", (Kind::Search, None, Some("vegan lasagna"), None)),
        ("look online for a used bike", (Kind::Search, None, Some("used bike"), None)),
        ("pull up netflix and log into my account", (Kind::None, None, None, None)), // a second step stays visible
    ];
    for (request, want) in expected {
        let r = expected_outcome(request, Some(&engine), Some(&no_env()));
        let got = r.as_ref().map(|r| (r.kind, r.site_host.as_deref(), r.query.as_deref(), r.n));
        assert_eq!(got, Some(want), "{request}: {r:?}");
    }
    engine.unload();
}

#[test]
#[ignore = "live: needs huggingface_hub (EVE_BROWSER_INTENT_PYTHON) and the cklxx/laya-browser checkpoint cached"]
fn test_the_real_checkpoint_is_located() {
    let path = cached_checkpoint().expect("cached");
    assert!(path.join(BROWSER_SUBFOLDER).join("model.safetensors").exists());
}

#[test]
fn test_a_site_search_said_with_another_verb() {
    for (said, h, query) in [
        ("browse air bnb for cabins in vermont", "airbnb.com", "cabins in vermont"),
        ("check ebay for a used camera", "ebay.com", "used camera"),
        ("find a floor lamp on ikea", "ikea.com", "floor lamp"),
        ("look for knitting patterns on etsy", "etsy.com", "knitting patterns"),
        ("shop for running shoes at target", "target.com", "running shoes"),
    ] {
        let a = literal_arguments(said);
        assert_eq!((host(&a), a.query.as_deref()), (Some(h), Some(query)), "{said}");
    }
}

#[test]
fn test_find_without_a_site_is_not_a_query() {
    for said in ["find my keys", "look for the remote", "check the oven", "browse my photos"] {
        assert!(literal_arguments(said).query.is_none(), "{said}");
    }
}

// ==== test_site_names.py (the browser_intent parts) ===========================================================

#[test]
fn test_split_site_names_are_joined() {
    for (said, joined) in [
        ("open git hub", "open GitHub"),
        ("search you tube for lofi beats", "search YouTube for lofi beats"),
        ("search duck duck go for privacy tools", "search DuckDuckGo for privacy tools"),
        ("go to stack overflow", "go to Stack Overflow"),
        ("open wiki pedia", "open Wikipedia"),
        ("what's new on red dit", "what's new on Reddit"),
        ("search e bay for a used camera", "search eBay for a used camera"),
        ("open linked in", "open LinkedIn"),
        ("put something on net flix", "put something on Netflix"),
        ("open hacker news", "open Hacker News"),
        ("Open Git Hub and search for laya", "Open GitHub and search for laya"),
        ("open YOU TUBE", "open YouTube"),
    ] {
        assert_eq!(join_site_names(said), joined, "{said}");
    }
}

#[test]
fn test_ordinary_words_are_not_joined() {
    for said in [
        "do you know the way",
        "meet me at the hub",
        "send an e mail to the team",
        "check my net income",
        "the red dot on the map",
        "the doc I linked in slack",
        "I hit a stack overflow error in my recursion",
        "a duck went for a swim",
        "news about hackers",
        "",
    ] {
        assert_eq!(join_site_names(said), said, "{said}");
    }
}

#[test]
fn test_hacker_news_stays_two_words_but_maps_to_its_host() {
    assert_eq!(site_for("Hacker News").unwrap().host, "news.ycombinator.com");
    assert_eq!(site_for("hacker news").unwrap().name, "Hacker News");
}

#[test]
fn test_the_table_maps_names_to_hosts_and_search_urls() {
    for (name, h) in [
        ("GitHub", "github.com"), ("YouTube", "youtube.com"), ("DuckDuckGo", "duckduckgo.com"),
        ("Stack Overflow", "stackoverflow.com"), ("Wikipedia", "wikipedia.org"), ("Reddit", "reddit.com"),
        ("eBay", "ebay.com"), ("LinkedIn", "linkedin.com"), ("Netflix", "netflix.com"),
    ] {
        assert!(site_for(name).is_some_and(|s| s.host == h), "{name}");
    }
    assert_eq!(
        site_for("youtube").unwrap().search_url("lofi beats"),
        "https://www.youtube.com/results?search_query=lofi+beats"
    );
    assert_eq!(site_for("GitHub").unwrap().search_url("a&b"), "https://github.com/search?q=a%26b&type=repositories");
    assert_eq!(site_for("Wikipedia").unwrap().query_key, "search");
    assert!(site_for("myspace").is_none());
}

#[test]
fn test_mentioned_site_is_found_after_joining() {
    assert_eq!(mentioned_site("search you tube for cats").unwrap().host, "youtube.com");
    assert_eq!(mentioned_site("go to stack overflow").unwrap().host, "stackoverflow.com");
    assert!(mentioned_site("I hit a stack overflow error").is_none());
    assert!(mentioned_site("set a timer").is_none());
}

#[test]
fn test_search_url_encodes_like_python_quote() {
    // path search: quote(safe=""), so "/" and "é" are escaped and "~" is kept
    assert_eq!(site_for("Spotify").unwrap().search_url("a/b é~"), "https://open.spotify.com/search/a%2Fb%20%C3%A9~");
    // query string: quote_plus
    assert_eq!(site_for("Bing").unwrap().search_url("a/b é~"), "https://www.bing.com/search?q=a%2Fb+%C3%A9~");
    assert_eq!(site_for("Expedia").unwrap().search_url("x"), "");
}
