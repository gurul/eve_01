//! Port of bridge/tests/test_spotify.py: the token, the Web API calls spotKnob makes, the code path, the tools,
//! the Mini App panel, the CLI. Plus the spotify half of test_music_mode.py.

use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;
use futures::future::BoxFuture;
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use eve_spotify as S;
use eve_spotify::{Command, ControlArgs, Dict, Http, HttpReply, Spotify, SpotifyApi, Token};

fn track() -> Value {
    json!({"name": "One More Time", "uri": "spotify:track:1", "duration_ms": 320000,
           "artists": [{"name": "Daft Punk"}],
           "album": {"name": "Discovery", "images": [{"url": "https://i.scdn.co/image/abc"}]}})
}
fn mac() -> Value {
    json!({"id": "dev-mac", "name": "Studio MacBook", "type": "Computer", "is_active": true, "volume_percent": 40})
}
fn kitchen() -> Value {
    json!({"id": "dev-kitchen", "name": "Kitchen Speaker", "type": "Speaker", "is_active": false, "volume_percent": 30})
}
fn tv() -> Value {
    json!({"id": "dev-tv", "name": "Living Room TV", "type": "TV", "is_active": false, "is_restricted": true})
}

type Call = (String, String, IndexMap<String, String>, Value);

struct State {
    calls: Vec<Call>,
    devices: Vec<Value>,
    playing: bool,
    item: Option<Value>,
    volume: i64,
    active: bool,
    premium: bool,
    expire_next: bool, // the next API call answers 401 once
    refreshes: usize,
    rotate: bool,
    search_hit: bool,
}

/// The Web API and the accounts service, enough of each to drive every path.
struct FakeSpotify {
    s: Mutex<State>,
}

fn reply(status: u16, body: Value) -> io::Result<HttpReply> {
    Ok(HttpReply::new(status, serde_json::to_vec(&body).unwrap()))
}

fn parse_q(query: Option<&str>) -> IndexMap<String, String> {
    let mut q = IndexMap::new();
    for (k, v) in url::form_urlencoded::parse(query.unwrap_or("").as_bytes()) {
        q.entry(k.into_owned()).or_insert_with(|| v.into_owned());
    }
    q
}

impl FakeSpotify {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            s: Mutex::new(State {
                calls: vec![],
                devices: vec![mac(), kitchen(), tv()],
                playing: true,
                item: Some(track()),
                volume: 40,
                active: true,
                premium: true,
                expire_next: false,
                refreshes: 0,
                rotate: false,
                search_hit: true,
            }),
        })
    }

    fn api(&self, path: &str) -> Vec<Call> {
        let want = format!("/v1{path}");
        self.s.lock().calls.iter().filter(|c| c.1 == want).cloned().collect()
    }

    fn answer(&self, method: &str, url: &str, body: Option<Vec<u8>>) -> io::Result<HttpReply> {
        let u = url::Url::parse(url).unwrap();
        let q = parse_q(u.query());
        let mut s = self.s.lock();
        if u.host_str() == Some("accounts.spotify.com") {
            let form: Map<String, Value> = url::form_urlencoded::parse(&body.unwrap_or_default())
                .map(|(k, v)| (k.into_owned(), json!(v.into_owned())))
                .collect();
            s.calls.push((method.into(), u.path().into(), q, Value::Object(form.clone())));
            if form.get("refresh_token") == Some(&json!("dead")) {
                return reply(400, json!({"error": "invalid_grant", "error_description": "Refresh token revoked"}));
            }
            s.refreshes += 1;
            let mut out = json!({"access_token": format!("at{}", s.refreshes), "expires_in": 3600});
            if s.rotate {
                out["refresh_token"] = json!(format!("rt{}", s.refreshes));
            }
            return reply(200, out);
        }
        let data = match &body {
            Some(b) if !b.is_empty() => serde_json::from_slice(b).unwrap(),
            _ => Value::Null,
        };
        s.calls.push((method.into(), u.path().into(), q.clone(), data));
        if s.expire_next {
            s.expire_next = false;
            return reply(401, json!({"error": {"status": 401, "message": "The access token expired"}}));
        }
        let p = u.path().strip_prefix("/v1").unwrap_or(u.path()).to_string();
        let no_device = json!({"error": {"status": 404, "message": "Player command failed: No active device found",
                                          "reason": "NO_ACTIVE_DEVICE"}});
        if p == "/me/player" && method == "GET" {
            if !s.active {
                return Ok(HttpReply::new(204, Vec::new()));
            }
            let mut device = mac();
            device["volume_percent"] = json!(s.volume);
            return reply(200, json!({"is_playing": s.playing, "progress_ms": 61000, "item": s.item, "device": device}));
        }
        if p == "/me/player/devices" {
            return reply(200, json!({"devices": s.devices}));
        }
        if !s.premium {
            return reply(403, json!({"error": {"status": 403, "message": "Premium required", "reason": "PREMIUM_REQUIRED"}}));
        }
        if p == "/search" {
            let kind = q["type"].clone();
            let items = if kind == "artist" { json!([{"name": "Daft Punk", "uri": "spotify:artist:dp"}]) } else { json!([track()]) };
            let items = if s.search_hit { items } else { json!([]) };
            return reply(200, json!({format!("{kind}s"): {"items": items}}));
        }
        if ["/me/player/play", "/me/player/pause", "/me/player/next", "/me/player/previous", "/me/player/volume"]
            .contains(&p.as_str())
        {
            if !s.active && !q.contains_key("device_id") {
                return reply(404, no_device);
            }
            if p.ends_with("play") {
                s.playing = true;
                s.active = true;
            } else if p.ends_with("pause") {
                s.playing = false;
            } else if p.ends_with("volume") {
                s.volume = q["volume_percent"].parse().unwrap();
            }
            return Ok(HttpReply::new(204, Vec::new()));
        }
        if p == "/me/player" && method == "PUT" {
            s.active = true;
            return Ok(HttpReply::new(204, Vec::new()));
        }
        Ok(HttpReply::new(404, b"{}".to_vec()))
    }
}

#[async_trait]
impl Http for FakeSpotify {
    async fn request(
        &self,
        method: &str,
        url: &str,
        _headers: &[(String, String)],
        body: Option<Vec<u8>>,
    ) -> io::Result<HttpReply> {
        self.answer(method, url, body)
    }
}

type HttpFn = dyn Fn(&str, &str, &[(String, String)], Option<Vec<u8>>) -> io::Result<HttpReply> + Send + Sync;

/// A transport from a closure.
struct FnHttp(Box<HttpFn>);

#[async_trait]
impl Http for FnHttp {
    async fn request(
        &self,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        body: Option<Vec<u8>>,
    ) -> io::Result<HttpReply> {
        (self.0)(method, url, headers, body)
    }
}

fn no_sleep() -> S::SleepFn {
    Arc::new(|_s: f64| -> BoxFuture<'static, ()> { Box::pin(async {}) })
}

fn cfg(pairs: &[(&str, &str)]) -> Dict {
    pairs.iter().map(|(k, v)| (k.to_string(), json!(v))).collect()
}

fn hub_with(fake: Arc<FakeSpotify>, wake: Option<S::WakeFn>) -> (Arc<Spotify>, Arc<FakeSpotify>) {
    let tok = Token::new(cfg(&[("client_id", "cid"), ("refresh_token", "rt0")]), fake.clone());
    let h = Spotify::new(SpotifyApi::new(tok, fake.clone()))
        .with_wake(wake.unwrap_or_else(|| Arc::new(|| false)))
        .with_sleep(no_sleep())
        .with_wake_secs(3.0);
    (Arc::new(h), fake)
}

fn hub() -> (Arc<Spotify>, Arc<FakeSpotify>) {
    hub_with(FakeSpotify::new(), None)
}

async fn ctl(h: &Spotify, action: &str, a: ControlArgs) -> Dict {
    h.control(action, a).await.unwrap()
}

async fn act(h: &Spotify, action: &str) -> Dict {
    ctl(h, action, ControlArgs::default()).await
}

fn line(d: &Dict) -> &str {
    d["line"].as_str().unwrap()
}

fn form_of(body: &[u8]) -> HashMap<String, String> {
    url::form_urlencoded::parse(body).map(|(k, v)| (k.into_owned(), v.into_owned())).collect()
}

// ---- the token ----

#[tokio::test]
async fn test_token_is_cached_then_refreshed_and_a_rotated_one_is_saved() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = FakeSpotify::new();
    fake.s.lock().rotate = true;
    let path = tmp.path().join("spotify.json");
    S::save(&cfg(&[("client_id", "cid"), ("refresh_token", "rt0")]), Some(&path)).unwrap();
    let now = Arc::new(Mutex::new(0.0f64));
    let clock: S::Clock = {
        let now = now.clone();
        Arc::new(move || *now.lock())
    };
    let tok = Token::with(S::load(Some(&path)), fake.clone(), Some(path.clone()), Some(clock));
    assert_eq!(tok.get(false).await.unwrap(), "at1");
    assert_eq!(tok.get(false).await.unwrap(), "at1"); // cached
    let form = fake.s.lock().calls[0].3.clone();
    assert_eq!(form, json!({"grant_type": "refresh_token", "refresh_token": "rt0", "client_id": "cid"})); // PKCE: no secret
    assert_eq!(S::load(Some(&path))["refresh_token"], json!("rt1")); // rotated, and kept at once
    *now.lock() = 3600.0 - S::TOKEN_MARGIN_SECS + 1.0;
    assert_eq!(tok.get(false).await.unwrap(), "at2");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
}

#[tokio::test]
async fn test_a_client_secret_goes_in_basic_auth_not_the_body() {
    type Seen = Vec<(Vec<(String, String)>, HashMap<String, String>)>;
    let seen: Arc<Mutex<Seen>> = Arc::default();
    let http = {
        let seen = seen.clone();
        FnHttp(Box::new(move |_m, _u, headers, body| {
            seen.lock().push((headers.to_vec(), form_of(&body.unwrap_or_default())));
            Ok(HttpReply::new(200, br#"{"access_token": "x", "expires_in": 3600}"#.to_vec()))
        }))
    };
    Token::new(cfg(&[("client_id", "cid"), ("client_secret", "sec"), ("refresh_token", "r")]), Arc::new(http))
        .get(false)
        .await
        .unwrap();
    let (headers, form) = seen.lock()[0].clone();
    let auth = headers.iter().find(|(k, _)| k == "Authorization").map(|(_, v)| v.clone()).unwrap();
    assert!(auth.starts_with("Basic "));
    assert!(!form.contains_key("client_id") && !format!("{form:?}").contains("sec"));
}

#[tokio::test]
async fn test_a_revoked_login_says_how_to_fix_it() {
    let fake = FakeSpotify::new();
    let tok = Token::new(cfg(&[("client_id", "cid"), ("refresh_token", "dead")]), fake.clone());
    let h = Spotify::new(SpotifyApi::new(tok, fake.clone())).with_wake(Arc::new(|| false)).with_sleep(no_sleep());
    let out = act(&h, "pause").await;
    assert_eq!(out["ok"], json!(false));
    assert!(line(&out).contains("spotify login") && line(&out).contains("Refresh token revoked"));
}

#[tokio::test]
async fn test_an_expired_access_token_is_refreshed_and_the_call_retried_once() {
    let (h, fake) = hub();
    fake.s.lock().expire_next = true;
    assert_eq!(act(&h, "pause").await["ok"], json!(true));
    assert_eq!(fake.api("/me/player/pause").len(), 2);
    assert_eq!(fake.s.lock().refreshes, 2);
}

#[test]
fn test_load_needs_an_id_and_a_token() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("s.json");
    assert!(S::load(Some(&p)).is_empty());
    std::fs::write(&p, r#"{"client_id": "x"}"#).unwrap();
    assert!(S::load(Some(&p)).is_empty());
    std::fs::write(&p, "not json").unwrap();
    assert!(S::load(Some(&p)).is_empty());
}

#[test]
fn test_pkce_pair_and_authorize_url() {
    let (v, c) = S::pkce_pair();
    assert!((43..=128).contains(&v.len()));
    assert_eq!(c, base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(v.as_bytes())));
    let url = url::Url::parse(&S::authorize_url("cid", &c, "st")).unwrap();
    let q = parse_q(url.query());
    assert_eq!(q["redirect_uri"], "http://127.0.0.1:8888/callback");
    assert_eq!(q["code_challenge_method"], "S256");
    let scopes: Vec<&str> = q["scope"].split_whitespace().collect();
    assert!(scopes.contains(&"user-read-playback-state") && scopes.contains(&"user-modify-playback-state"));
}

// ---- the knob's powers ----

#[tokio::test]
async fn test_now_playing_is_trimmed_and_names_the_device() {
    let (h, _) = hub();
    let now = h.now_playing().await.unwrap();
    assert_eq!(now["ok"], json!(true));
    assert_eq!(now["track"], json!("One More Time"));
    assert_eq!(now["artists"], json!(["Daft Punk"]));
    assert_eq!(now["art"], json!("https://i.scdn.co/image/abc"));
    assert_eq!(now["device"]["volume"], json!(40));
    let names: Vec<&str> = now["devices"].as_array().unwrap().iter().map(|d| d["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Studio MacBook", "Kitchen Speaker", "Living Room TV"]);
    assert_eq!(now["devices"][2]["restricted"], json!(true));
    assert!(now["devices"][0].get("id").is_none());
    assert_eq!(S::said_now(&now), "Playing One More Time by Daft Punk on Studio MacBook.");
}

#[test]
fn test_art_from_anywhere_else_is_dropped() {
    let mut item = track();
    item["album"] = json!({"name": "x", "images": [{"url": "http://evil.example/a.png"}]});
    assert!(!S::now_of(Some(&json!({"is_playing": true, "item": item}))).contains_key("art"));
}

#[tokio::test]
async fn test_nothing_playing() {
    let (h, fake) = hub();
    fake.s.lock().active = false;
    let now = h.now_playing().await.unwrap();
    assert_eq!(now["ok"], json!(true));
    assert_eq!(now["track"], Value::Null);
    assert_eq!(S::said_now(&now), "Nothing is playing on Spotify.");
}

#[tokio::test]
async fn test_pause_play_next_previous() {
    let (h, fake) = hub();
    assert_eq!(line(&act(&h, "pause").await), "Paused.");
    assert!(!fake.s.lock().playing);
    assert_eq!(line(&act(&h, "play").await), "Playing.");
    assert!(fake.s.lock().playing);
    assert_eq!(line(&act(&h, "next").await), "Skipped. Now One More Time by Daft Punk.");
    assert!(line(&act(&h, "previous").await).starts_with("Back one."));
    let methods: Vec<String> = fake.api("/me/player/next").into_iter().map(|c| c.0).collect();
    assert_eq!(methods, ["POST"]);
}

fn vol(v: i64) -> ControlArgs {
    ControlArgs { volume: Some(v), ..ControlArgs::default() }
}
fn change(v: i64) -> ControlArgs {
    ControlArgs { volume_change: Some(v), ..ControlArgs::default() }
}
fn on_device(d: &str) -> ControlArgs {
    ControlArgs { device: d.into(), ..ControlArgs::default() }
}

#[tokio::test]
async fn test_volume_absolute_relative_and_clamped() {
    let (h, fake) = hub();
    assert_eq!(line(&ctl(&h, "volume", vol(65)).await), "Spotify volume 65%.");
    assert_eq!(ctl(&h, "volume", change(10)).await["volume"], json!(75));
    assert_eq!(ctl(&h, "volume", change(-100)).await["volume"], json!(0));
    assert_eq!(ctl(&h, "volume", vol(250)).await["volume"], json!(100));
    assert_eq!(fake.s.lock().volume, 100);
    assert_eq!(act(&h, "volume").await["ok"], json!(false));
}

#[tokio::test]
async fn test_transfer_picks_a_device_by_name_word_or_kind() {
    let (h, fake) = hub();
    assert_eq!(line(&ctl(&h, "transfer", on_device("the kitchen speaker")).await), "Playing on Kitchen Speaker.");
    assert_eq!(fake.api("/me/player").last().unwrap().3, json!({"device_ids": ["dev-kitchen"], "play": true}));
    assert_eq!(ctl(&h, "transfer", on_device("kitchen")).await["ok"], json!(true));
    assert_eq!(line(&ctl(&h, "transfer", on_device("mac")).await), "Playing on Studio MacBook.");
    let tv = ctl(&h, "transfer", on_device("living room tv")).await;
    assert_eq!(tv["ok"], json!(false));
    assert!(line(&tv).contains("remote control"));
    let none = ctl(&h, "transfer", on_device("garage")).await;
    assert_eq!(none["ok"], json!(false));
    assert!(line(&none).contains("Kitchen Speaker") && !line(&none).contains("Living Room TV"));
}

#[tokio::test]
async fn test_play_by_name_searches_then_plays_the_track_or_the_context() {
    let (h, fake) = hub();
    let out = ctl(&h, "play", ControlArgs { query: "one more time".into(), ..ControlArgs::default() }).await;
    assert_eq!(line(&out), "Playing One More Time by Daft Punk.");
    assert_eq!(fake.api("/me/player/play").last().unwrap().3, json!({"uris": ["spotify:track:1"]}));
    let a = ControlArgs { query: "daft punk".into(), kind: "artist".into(), device: "kitchen".into(), ..ControlArgs::default() };
    let out = ctl(&h, "play", a).await;
    assert_eq!(line(&out), "Playing Daft Punk on Kitchen Speaker.");
    let last = fake.api("/me/player/play").last().unwrap().clone();
    assert_eq!(last.3, json!({"context_uri": "spotify:artist:dp"}));
    assert_eq!(last.2, IndexMap::from([("device_id".to_string(), "dev-kitchen".to_string())]));
    fake.s.lock().search_hit = false;
    let out = ctl(&h, "play", ControlArgs { query: "zzzz".into(), ..ControlArgs::default() }).await;
    assert_eq!(line(&out), "Spotify found nothing for 'zzzz'.");
}

#[tokio::test]
async fn test_play_with_nothing_active_starts_on_the_mac() {
    let (h, fake) = hub();
    fake.s.lock().active = false;
    assert_eq!(act(&h, "play").await["ok"], json!(true));
    let plays = fake.api("/me/player/play");
    assert!(plays[0].2.is_empty());
    assert_eq!(plays[1].2, IndexMap::from([("device_id".to_string(), "dev-mac".to_string())]));
}

#[tokio::test]
async fn test_play_with_no_device_online_opens_the_mac_app_and_waits() {
    let fake = FakeSpotify::new();
    {
        let mut s = fake.s.lock();
        s.active = false;
        s.devices = vec![];
    }
    let woke = Arc::new(AtomicUsize::new(0));
    let wake: S::WakeFn = {
        let (woke, fake) = (woke.clone(), fake.clone());
        Arc::new(move || {
            woke.fetch_add(1, Ordering::SeqCst);
            fake.s.lock().devices = vec![mac()]; // the app comes online
            true
        })
    };
    let (h, _) = hub_with(fake.clone(), Some(wake));
    assert_eq!(act(&h, "play").await["ok"], json!(true));
    assert_eq!(woke.load(Ordering::SeqCst), 1);
    // no Mac app to open: says what to do
    fake.s.lock().devices = vec![];
    let (h2, _) = hub_with(fake.clone(), Some(Arc::new(|| false)));
    fake.s.lock().active = false;
    let out = act(&h2, "play").await;
    assert_eq!(out["ok"], json!(false));
    assert!(line(&out).contains("Open Spotify"));
}

#[tokio::test]
async fn test_refusals_in_plain_words() {
    let (h, fake) = hub();
    fake.s.lock().premium = false;
    assert_eq!(line(&act(&h, "next").await), "Controlling playback needs Spotify Premium.");
    let (h, fake) = hub();
    fake.s.lock().active = false;
    assert_eq!(line(&act(&h, "pause").await), "Spotify is already quiet.");

    let down: Arc<dyn Http> = Arc::new(FnHttp(Box::new(|_, _, _, _| Err(io::Error::other("no route to host")))));
    let h = Spotify::new(SpotifyApi::new(Token::new(cfg(&[("client_id", "c"), ("refresh_token", "r")]), down.clone()), down))
        .with_sleep(no_sleep());
    assert_eq!(line(&act(&h, "pause").await), "Couldn't reach Spotify.");
}

// ---- the code path ----

fn cmd(action: &str) -> Command {
    Command::new(action)
}

#[test]
fn test_match() {
    let cases: Vec<(&str, Command)> = vec![
        ("pause the music", cmd("pause")),
        ("Stop music please", cmd("pause")),
        ("hey eve, pause spotify", cmd("pause")),
        ("play music", cmd("play")),
        ("resume spotify", cmd("play")),
        ("play music on Spotify", cmd("play")),
        ("next song", cmd("next")),
        ("skip this song", cmd("next")),
        ("previous track", cmd("previous")),
        ("music volume 40", Command { volume: Some(40), ..cmd("volume") }),
        ("spotify volume to 45%", Command { volume: Some(45), ..cmd("volume") }),
        ("set the volume of the music to 30", Command { volume: Some(30), ..cmd("volume") }),
        ("turn the music up", Command { change: Some(S::VOLUME_STEP), ..cmd("volume") }),
        ("music quieter", Command { change: Some(-S::VOLUME_STEP), ..cmd("volume") }),
        ("what's playing?", cmd("now")),
        ("what song is this", cmd("now")),
        ("play the music on the kitchen speaker please", Command { device: "kitchen speaker".into(), ..cmd("transfer") }),
        ("move spotify to my phone", Command { device: "phone".into(), ..cmd("transfer") }),
        ("play Daft Punk on Spotify", Command { query: "daft punk".into(), ..cmd("play") }),
    ];
    let (h, _) = hub();
    for (text, want) in cases {
        assert_eq!(h.match_text(text), Some(want), "{text}");
    }
}

#[test]
fn test_match_leaves_the_rest_to_the_model() {
    let (h, _) = hub();
    for text in [
        "pause", "skip", "next", "volume 40", "turn the volume down", "music volume 400", "what is this", "what's on",
        "pause the music in ten minutes", "play some jazz", "turn off the lights", "stop", "what's the weather",
    ] {
        assert_eq!(h.match_text(text), None, "{text}");
    }
}

#[tokio::test]
async fn test_run_by_code_says_what_happened() {
    let (h, _) = hub();
    assert_eq!(h.run(&cmd("now")).await.unwrap(), "Playing One More Time by Daft Punk on Studio MacBook.");
    assert_eq!(h.run(&Command { volume: Some(20), ..cmd("volume") }).await.unwrap(), "Spotify volume 20%.");
}

// ---- the tools ----

#[test]
fn test_tools_are_strict_and_complete() {
    for t in S::TOOLS.iter() {
        let params = &t["parameters"];
        assert_eq!(t["strict"], json!(true));
        assert_eq!(params["additionalProperties"], json!(false));
        let mut req: Vec<&str> = params["required"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        let mut props: Vec<&str> = params["properties"].as_object().unwrap().keys().map(String::as_str).collect();
        req.sort_unstable();
        props.sort_unstable();
        assert_eq!(req, props);
    }
    let names: Vec<&str> = S::TOOLS.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, S::TOOL_NAMES);
    assert_eq!(S::TOOL_NAMES, ["spotify_now_playing", "spotify_control"]);
}

fn args(v: Value) -> Dict {
    v.as_object().unwrap().clone()
}

#[tokio::test]
async fn test_handle() {
    let (h, _) = hub();
    assert_eq!(h.handle("spotify_now_playing", &Dict::new()).await.unwrap()["track"], json!("One More Time"));
    let a = args(json!({"action": "volume", "query": null, "kind": null, "volume": null, "volume_change": -10, "device": null}));
    assert_eq!(h.handle("spotify_control", &a).await.unwrap()["volume"], json!(30));
    let mut a = args(json!({"action": "play", "query": "daft punk", "kind": "artist", "volume": null,
                            "volume_change": null, "device": null}));
    assert_eq!(h.handle("spotify_control", &a).await.unwrap()["ok"], json!(true));
    a.insert("kind".into(), json!("podcast"));
    assert_eq!(h.handle("spotify_control", &a).await.unwrap()["ok"], json!(false));
    assert_eq!(h.handle("nope", &Dict::new()).await.unwrap()["ok"], json!(false));
}

// ---- the Mini App panel ----

#[tokio::test]
async fn test_panel_answers_with_fresh_state_and_refuses_search() {
    let (h, _) = hub();
    let st = h.panel(&args(json!({"action": "status"}))).await.unwrap();
    assert_eq!(st["ok"], json!(true));
    assert_eq!(st["playing"], json!(true));
    let paused = h.panel(&args(json!({"action": "pause"}))).await.unwrap();
    assert_eq!(paused["ok"], json!(true));
    assert_eq!(paused["line"], json!("Paused."));
    assert_eq!(paused["playing"], json!(false));
    let moved = h.panel(&args(json!({"action": "transfer", "device": "Kitchen Speaker"}))).await.unwrap();
    assert_eq!(moved["line"], json!("Playing on Kitchen Speaker."));
    assert!(!moved["devices"].as_array().unwrap().is_empty());
    // the card only drives
    assert_eq!(h.panel(&args(json!({"action": "play", "query": "anything"}))).await.unwrap()["ok"], json!(false));
    assert_eq!(h.panel(&args(json!({"action": "rm -rf"}))).await.unwrap()["ok"], json!(false));
}

// ---- construction and the CLI ----

#[test]
fn test_make_spotify_off_switch_and_no_login() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("spotify.json");
    let empty = HashMap::new();
    assert!(S::make_spotify(Some(&p), Some(&empty), None).is_none());
    S::save(&cfg(&[("client_id", "c"), ("refresh_token", "r")]), Some(&p)).unwrap();
    let off = HashMap::from([("EVE_SPOTIFY".to_string(), "0".to_string())]);
    assert!(S::make_spotify(Some(&p), Some(&off), None).is_none());
    assert!(S::make_spotify(Some(&p), Some(&empty), None).is_some());
}

fn argv(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

#[tokio::test]
async fn test_cli_status_volume_and_not_logged_in() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("spotify.json");
    let mut lines: Vec<String> = vec![];
    assert_eq!(S::cli(&argv(&["status"]), Some(&p), &mut |l| lines.push(l), None).await, 1);
    assert!(lines[0].contains("spotify login"));
    S::save(&cfg(&[("client_id", "c"), ("refresh_token", "r")]), Some(&p)).unwrap();
    let fake = FakeSpotify::new();
    lines.clear();
    assert_eq!(S::cli(&argv(&["status"]), Some(&p), &mut |l| lines.push(l), Some(fake.clone())).await, 0);
    assert!(lines[0].starts_with("Playing One More Time"));
    assert!(lines.iter().any(|x| x.contains("Kitchen Speaker")));
    assert!(lines.iter().any(|x| x.contains("no remote control")));
    lines.clear();
    assert_eq!(S::cli(&argv(&["volume", "55"]), Some(&p), &mut |l| lines.push(l), Some(fake.clone())).await, 0);
    assert_eq!(fake.s.lock().volume, 55);
    if std::env::var("SPOTIFY_CLIENT_ID").unwrap_or_default().is_empty() {
        assert_eq!(S::cli(&argv(&["login"]), Some(&p), &mut |l| lines.push(l), None).await, 2); // no client id
    }
    assert_eq!(S::cli(&argv(&["logout"]), Some(&p), &mut |l| lines.push(l), None).await, 0);
    assert!(!p.exists());
}

#[tokio::test]
async fn test_pausing_what_is_already_paused_is_not_a_failure() {
    let fake = FakeSpotify::new();
    let http: Arc<dyn Http> = {
        let fake = fake.clone();
        Arc::new(FnHttp(Box::new(move |method, url, _h, body| {
            if url.ends_with("/me/player/pause") {
                fake.s.lock().playing = false;
                return Ok(HttpReply::new(
                    403,
                    br#"{"error": {"status": 403, "message": "Player command failed: Restriction violated", "reason": "UNKNOWN"}}"#.to_vec(),
                ));
            }
            fake.answer(method, url, body)
        })))
    };
    let tok = Token::new(cfg(&[("client_id", "cid"), ("refresh_token", "rt0")]), fake.clone());
    let h = Spotify::new(SpotifyApi::new(tok, http)).with_sleep(no_sleep()).with_wake(Arc::new(|| false));
    assert_eq!(line(&act(&h, "pause").await), "Spotify is already quiet.");
}

/// The browser half is simulated: the redirect is sent to the loopback server the login opened.
#[tokio::test]
async fn test_login_trades_the_code_with_the_injected_transport() {
    let got: Arc<Mutex<Vec<HashMap<String, String>>>> = Arc::default();
    let http: Arc<dyn Http> = {
        let got = got.clone();
        Arc::new(FnHttp(Box::new(move |_m, _u, _h, body| {
            got.lock().push(form_of(&body.unwrap_or_default()));
            Ok(HttpReply::new(200, br#"{"access_token": "a", "refresh_token": "r-new", "expires_in": 3600}"#.to_vec()))
        })))
    };
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap(); // any free port
    let port = listener.local_addr().unwrap().port();
    let browser = move |url: &str| {
        let state = parse_q(url::Url::parse(url).unwrap().query())["state"].clone();
        std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};
            let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(s, "GET /callback?code=c0de&state={state} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
            let mut page = String::new();
            let _ = s.read_to_string(&mut page);
        });
    };
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("spotify.json");
    let mut lines: Vec<String> = vec![];
    let code = S::login_on(listener, "cid", "", Some(&path), http, &mut |l| lines.push(l), Some(&browser), 10.0).await;
    assert_eq!(code, 0, "{lines:?}");
    let form = got.lock()[0].clone();
    assert_eq!(form["code"], "c0de");
    assert_eq!(form["client_id"], "cid");
    assert!(!form["code_verifier"].is_empty());
    assert_eq!(
        Value::Object(S::load(Some(&path))),
        json!({"client_id": "cid", "refresh_token": "r-new", "scopes": S::SCOPES})
    );
}

// ---- each device's colour ----

#[test]
fn test_each_device_keeps_its_own_colour_and_green_and_red_are_never_used() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("spotify-colors.json");
    let mut c = S::DeviceColors::new(Some(&path));
    let (mac, tv, phone) = (c.hex("MacBook"), c.hex("TV"), c.hex("Phone"));
    assert!(mac != tv && tv != phone && mac != phone); // distinct
    assert_eq!(S::DeviceColors::new(Some(&path)).hex("TV"), tv); // kept across restarts
    let speaker = S::DeviceColors::new(Some(&path)).hex("Speaker");
    assert!(![&mac, &tv, &phone].contains(&&speaker)); // a new one takes a free colour
    for (_, (r, g, b)) in S::DEVICE_COLORS {
        assert!(!(g > 150 && r < 100 && b < 150)); // no green: Spotify mode's
        assert!(!(r > 150 && g < 60 && b < 60)); // no red: a refusal's
    }
    for i in 0..20 {
        // past seven, colours repeat evenly
        c.index(&format!("extra {i}"));
    }
    let counts: Vec<usize> = (0..S::DEVICE_COLORS.len() as i64)
        .map(|i| c.known.values().filter(|e| e.i == i).count())
        .collect();
    assert!(counts.iter().max().unwrap() - counts.iter().min().unwrap() <= 1);
}

#[tokio::test]
async fn test_now_playing_gives_each_device_its_colour() {
    let (h, _) = hub();
    let now = h.now_playing().await.unwrap();
    let devices = now["devices"].as_array().unwrap();
    for d in devices {
        assert_eq!(d["color"].as_str().unwrap(), h.colors.lock().hex(d["name"].as_str().unwrap()));
    }
    let mut colors: Vec<&str> = devices.iter().map(|d| d["color"].as_str().unwrap()).collect();
    colors.sort_unstable();
    colors.dedup();
    assert_eq!(colors.len(), devices.len());
}

#[test]
fn test_a_broken_colours_file_starts_fresh() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("c.json");
    std::fs::write(&path, "not json").unwrap();
    let (r, g, b) = S::DEVICE_COLORS[0].1;
    assert_eq!(S::DeviceColors::new(Some(&path)).hex("A"), format!("#{r:02x}{g:02x}{b:02x}"));
}

fn day_clock(day: &Arc<AtomicI64>) -> S::Today {
    let day = day.clone();
    Arc::new(move || day.load(Ordering::SeqCst))
}

#[test]
fn test_a_device_not_seen_for_30_days_gives_its_colour_back() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("spotify-colors.json");
    let day = Arc::new(AtomicI64::new(20000));
    let mut c = S::DeviceColors::with(Some(&path), day_clock(&day), S::FORGET_DEVICE_DAYS);
    let (mac, guest) = (c.index("MacBook"), c.index("Guest phone"));
    day.fetch_add(20, Ordering::SeqCst);
    assert_eq!(c.index("MacBook"), mac); // seen again: its day moves on
    day.fetch_add(15, Ordering::SeqCst); // the guest: 35 days unseen; the Mac: 15
    let new = S::DeviceColors::with(Some(&path), day_clock(&day), S::FORGET_DEVICE_DAYS).index("New speaker");
    assert_eq!(new, guest); // the guest's colour is free again
    let kept: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(kept.get("Guest phone").is_none());
    assert_eq!(kept["MacBook"]["i"], json!(mac));
}

#[test]
fn test_the_first_colours_file_shape_still_reads() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("spotify-colors.json");
    std::fs::write(&path, r#"{"MacBook": 0, "TV": 1}"#).unwrap();
    let day = Arc::new(AtomicI64::new(20000));
    let mut c = S::DeviceColors::with(Some(&path), day_clock(&day), S::FORGET_DEVICE_DAYS);
    assert_eq!(c.index("MacBook"), 0);
    assert_eq!(c.index("TV"), 1);
    assert_eq!(c.index("Phone"), 2);
}

#[test]
fn test_the_colours_file_is_written_as_python_writes_it() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("spotify-colors.json");
    let day = Arc::new(AtomicI64::new(20000));
    S::DeviceColors::with(Some(&path), day_clock(&day), S::FORGET_DEVICE_DAYS).index("Café");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\n  \"Caf\\u00e9\": {\n    \"i\": 0,\n    \"seen\": 20000\n  }\n}");
}

// ---- test_music_mode.py: saying it, spotify.py's mode commands ----

#[tokio::test]
async fn test_spotify_mode_by_words_and_by_the_tool() {
    let (h, _) = hub();
    assert_eq!(h.match_text("spotify mode"), Some(Command { on: Some(true), ..cmd("mode") }));
    assert_eq!(h.match_text("turn off spotify mode"), Some(Command { on: Some(false), ..cmd("mode") }));
    assert_eq!(h.match_text("exit spotify mode"), Some(Command { on: Some(false), ..cmd("mode") }));
    assert_eq!(h.match_text("what is spotify mode"), None);
    // without a Voice PE, it says so
    assert!(h.run(&Command { on: Some(true), ..cmd("mode") }).await.unwrap().contains("Voice PE"));
    let sent: Arc<Mutex<Vec<Value>>> = Arc::default();
    let send: eve_music_mode::SendFn = {
        let sent = sent.clone();
        Arc::new(move |v: Value| -> BoxFuture<'static, anyhow::Result<()>> {
            sent.lock().push(v);
            Box::pin(async { Ok(()) })
        })
    };
    let mode = Arc::new(eve_music_mode::MusicMode::new(h.clone(), send, None));
    h.set_mode(Some(mode.clone()));
    assert!(h.run(&Command { on: Some(true), ..cmd("mode") }).await.unwrap().starts_with("Spotify mode on"));
    let a = args(json!({"action": "mode_off", "query": null, "kind": null, "volume": null, "volume_change": null, "device": null}));
    assert_eq!(h.handle("spotify_control", &a).await.unwrap()["line"], json!("Spotify mode off."));
    assert!(!mode.on());
    assert!(sent.lock().contains(&json!({"cmd": "music_mode", "on": true})));
}

#[tokio::test]
async fn test_the_panel_reports_the_mode() {
    let (h, _) = hub();
    assert_eq!(h.panel(&Dict::new()).await.unwrap()["mode"], Value::Null);
    let send: eve_music_mode::SendFn = Arc::new(|_v: Value| -> BoxFuture<'static, anyhow::Result<()>> { Box::pin(async { Ok(()) }) });
    h.set_mode(Some(Arc::new(eve_music_mode::MusicMode::new(h.clone(), send, None))));
    let out = h.panel(&args(json!({"action": "mode", "on": true}))).await.unwrap();
    assert_eq!(out["ok"], json!(true));
    assert_eq!(out["mode"], json!(true));
    assert!(out["line"].as_str().unwrap().starts_with("Spotify mode on"));
}
