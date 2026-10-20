//! test_phone_location.py's "quick answers' here" section: the phone's fresh fix is "here", rounded and named; a
//! stale fix or the switch off falls back to the configured place. Coordinates are fictional (the repository is
//! public). Each test keeps its fix in its own temp file, named by EVE_PHONE_LOCATION_FILE in the lent environment.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{Duration, NaiveDate, NaiveDateTime, Utc};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

use eve_places::phone_location as pl;
use eve_quick_answers as qa;
use eve_quick_answers::QuickAnswers;

fn noon() -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 10, 12).unwrap().and_hms_opt(12, 0, 0).unwrap()
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

fn fix(minutes_ago: f64, lat: f64, lon: f64, place: Option<&str>) -> pl::Fix {
    let at = Utc::now() - Duration::microseconds((minutes_ago * 60e6) as i64);
    pl::Fix::new(lat, lon, 10.0, at, place)
}

struct Home {
    _dir: tempfile::TempDir,
    file: std::path::PathBuf,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("phone-location.json");
        Home { _dir: dir, file }
    }

    fn store(&self, f: &pl::Fix) {
        assert!(pl::store(f, Some(&self.file)).unwrap());
    }

    fn env(&self, pairs: &[(&str, &str)]) -> HashMap<String, String> {
        let mut env: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        env.insert(pl::FILE_ENV.to_string(), self.file.to_string_lossy().into_owned());
        env
    }
}

type Calls = Arc<Mutex<Vec<(String, Map<String, Value>)>>>;

#[derive(Clone, Default)]
struct Api {
    calls: Calls,
}

impl Api {
    fn getter(&self) -> qa::Getter {
        let api = self.clone();
        qa::getter(move |url: String, params: Map<String, Value>| {
            let api = api.clone();
            async move {
                api.calls.lock().push((url.clone(), params));
                if url == qa::GEOCODE_URL {
                    return Ok(json!({"results": [{"name": "Springfield", "latitude": 10.0, "longitude": 20.0}]}));
                }
                Ok(json!({"current": {"temperature_2m": 60, "apparent_temperature": 60, "weather_code": 0,
                                      "wind_speed_10m": 3},
                          "daily": {"temperature_2m_min": [50, 50], "temperature_2m_max": [70, 70],
                                    "precipitation_probability_max": [0, 0], "weather_code": [0, 0],
                                    "sunrise": ["2026-10-12T07:00", "2026-10-13T07:01"],
                                    "sunset": ["2026-10-12T18:30", "2026-10-13T18:29"]}}))
            }
        })
    }

    fn last_coords(&self) -> (f64, f64) {
        let calls = self.calls.lock();
        let p = &calls.last().unwrap().1;
        (p["latitude"].as_f64().unwrap(), p["longitude"].as_f64().unwrap())
    }
}

fn quick(env: HashMap<String, String>, api: &Api) -> QuickAnswers {
    QuickAnswers::new(Some(env)).with_get(api.getter()).with_now(|_| noon())
}

fn ask(q: &QuickAnswers, text: &str) -> Option<String> {
    let a = q.match_text(text)?;
    rt().block_on(q.answer(&a))
}

#[test]
fn test_a_fresh_phone_fix_is_here_rounded_and_named() {
    let home = Home::new();
    home.store(&fix(5.0, 37.774912, -122.419412, Some("San Francisco, CA")));
    let api = Api::default();
    let q = quick(home.env(&[("EVE_WEATHER_LAT", "10"), ("EVE_WEATHER_LON", "20"), ("EVE_WEATHER_PLACE", "Springfield")]), &api);
    let line = ask(&q, "what's the weather").unwrap();
    assert!(line.starts_with("In San Francisco, CA it's 60°F"), "{line}");
    assert_eq!(api.last_coords(), (37.77, -122.42));
    assert_eq!(ask(&q, "when is sunset").as_deref(), Some("Sunset in San Francisco, CA is at 6:30 PM today."));
    assert!(api.calls.lock().iter().all(|(u, _)| u != qa::GEOCODE_URL));
}

#[test]
fn test_with_no_place_name_it_is_where_you_are() {
    let home = Home::new();
    home.store(&fix(5.0, 0.0, 0.0, None));
    let q = quick(home.env(&[]), &Api::default());
    assert!(q.match_text("what's the weather").is_some()); // no configured home: the phone is "here"
    assert!(ask(&q, "what's the weather").unwrap().starts_with(&format!("In {} it's", qa::PHONE_PLACE)));
}

#[test]
fn test_a_stale_fix_or_the_switch_off_falls_back_to_the_configured_place() {
    let home = Home::new();
    let configured = [("EVE_WEATHER_LAT", "10"), ("EVE_WEATHER_LON", "20"), ("EVE_WEATHER_PLACE", "Springfield")];
    home.store(&fix(61.0, 0.0, 0.0, Some("San Francisco, CA")));
    let api = Api::default();
    let q = quick(home.env(&configured), &api);
    assert!(ask(&q, "what's the weather").unwrap().starts_with("In Springfield it's"));
    assert_eq!(api.last_coords(), (10.0, 20.0));
    assert_eq!(quick(home.env(&[]), &Api::default()).match_text("what's the weather"), None); // stale and no home
    std::fs::remove_file(&home.file).unwrap();
    home.store(&fix(1.0, 0.0, 0.0, Some("San Francisco, CA")));
    assert!(ask(&q, "what's the weather").unwrap().starts_with("In San Francisco, CA it's"));
    let mut off_env = home.env(&configured);
    off_env.insert(pl::SWITCH.to_string(), "0".to_string()); // the switch, from the environment it was given
    let off = quick(off_env, &Api::default());
    assert!(ask(&off, "what's the weather").unwrap().starts_with("In Springfield it's"));
    let mut off_env = home.env(&[]);
    off_env.insert(pl::SWITCH.to_string(), "0".to_string());
    assert_eq!(quick(off_env, &Api::default()).match_text("what's the weather"), None);
}

#[test]
fn test_a_named_place_still_beats_the_phone() {
    let home = Home::new();
    home.store(&fix(1.0, 0.0, 0.0, Some("San Francisco, CA")));
    let q = quick(home.env(&[]), &Api::default());
    assert!(ask(&q, "what's the weather in springfield").unwrap().starts_with("In Springfield it's"));
}

#[test]
fn test_a_broken_phone_reader_never_breaks_an_answer() {
    let env: HashMap<String, String> =
        [("EVE_WEATHER_LAT", "10"), ("EVE_WEATHER_LON", "20")].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let q = QuickAnswers::new(Some(env))
        .with_get(Api::default().getter())
        .with_phone(|| Err(anyhow::anyhow!("no")))
        .with_now(|_| noon());
    assert!(ask(&q, "what's the weather").unwrap().starts_with("In your area it's"));
}

#[test]
fn a_phone_fix_is_here_for_phone_here_only_while_fresh() {
    let home = Home::new();
    home.store(&fix(5.0, 37.774912, -122.419412, Some("San Francisco, CA")));
    let env = home.env(&[]);
    let here = qa::phone_here(Some(&env)).unwrap();
    assert_eq!((here.name.as_str(), here.lat, here.lon, here.tz.clone()), ("San Francisco, CA", 37.77, -122.42, None));
    std::fs::remove_file(&home.file).unwrap();
    home.store(&fix(61.0, 0.0, 0.0, None));
    assert_eq!(qa::phone_here(Some(&env)), None);
}
