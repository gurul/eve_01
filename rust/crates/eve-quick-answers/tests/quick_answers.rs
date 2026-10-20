//! test_quick_answers.py: standard questions answered by code. What matches, what is left to the model, the words,
//! and the fall-back to the model when a source fails. Open-Meteo is faked; nothing here touches the network.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{NaiveDate, NaiveDateTime};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

use eve_quick_answers as qa;
use eve_quick_answers::{Args, Ask, Kind, Num, QuickAnswers};

fn now() -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 9, 26).unwrap().and_hms_opt(16, 5, 0).unwrap()
}

fn date(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

#[test]
fn test_standard_questions_are_answered_by_code() {
    let cases = [
        ("What time is it?", "time"),
        ("hey eve, what's the time", "time"),
        ("what time is it right now", "time"),
        ("Eve what time is it", "time"),
        ("can you tell me what time it is please", "time"),
        ("what's the date", "date"),
        ("what day is it today?", "date"),
        ("what's today's date", "date"),
        ("what's the weather", "weather"),
        ("how's the weather outside", "weather"),
        ("weather tomorrow", "weather"),
        ("is it going to rain today", "weather"),
        ("will it rain tomorrow?", "weather"),
        ("how cold is it", "weather"),
        ("do I need an umbrella", "weather"),
        ("what's the temperature outside", "weather"),
        ("when is sunset", "sun"),
        ("what time is sunrise tomorrow", "sun"),
        ("when does the sun set", "sun"),
        ("what's the air quality", "air"),
        ("aqi", "air"),
        ("what's my battery", "battery"),
        ("how much battery do I have left", "battery"),
        ("how many days until christmas", "countdown"),
        ("how long until Thanksgiving", "countdown"),
        ("what's 17 times 23", "math"),
        ("15 percent of 80", "math"),
        ("what is 2 to the power of 10", "math"),
        ("convert 5 km to miles", "convert"),
        ("72 fahrenheit in celsius", "convert"),
        ("how many ounces in a pound", "convert"),
        ("what's 3 cups in ml", "convert"),
    ];
    for (text, kind) in cases {
        let ask = qa::match_text(text);
        assert!(ask.as_ref().is_some_and(|a| a.kind.as_str() == kind), "{text}: {ask:?}");
    }
}

#[test]
fn test_anything_more_goes_to_the_model() {
    for text in [
        "what time is my meeting",
        "what time does the pharmacy close",
        "should I bike given the weather",
        "what's 17 times my rent",
        "remind me what the date of the party is",
        "what's 9/11",
        "open safari",
        "what's the weather been like this week in my photos",
        "how many days until my birthday",
        "time to go",
        "what is the time complexity of quicksort",
        "convert this pdf to word",
        "how many cups of coffee did I have",
        "what's 2026",
        "tell me about the weather in ancient rome and why",
    ] {
        assert_eq!(qa::match_text(text), None, "{text}");
    }
}

#[test]
fn test_a_place_and_a_day_are_read() {
    assert_eq!(
        qa::match_text("what's the weather in Tokyo tomorrow"),
        Some(Ask::new(Kind::Weather).with_place("tokyo").with_day(1))
    );
    assert_eq!(qa::match_text("what time is it in New York"), Some(Ask::new(Kind::Time).with_place("new york")));
    assert_eq!(qa::match_text("air quality in delhi"), Some(Ask::new(Kind::Air).with_place("delhi")));
}

fn approx(a: f64, b: f64, rel: f64) -> bool {
    (a - b).abs() <= (rel * b.abs()).max(1e-12)
}

#[test]
fn test_the_sums_are_right() {
    let cases = [
        ("what's 17 times 23", 391.0),
        ("12 plus 30", 42.0),
        ("100 minus 1", 99.0),
        ("144 divided by 12", 12.0),
        ("15 percent of 80", 12.0),
        ("2 to the power of 10", 1024.0),
        ("what's 1,000 times 3", 3000.0),
        ("9 squared", 81.0),
        ("the square root of 81", 9.0),
        ("7 x 6", 42.0),
        ("(2 + 3) * 4", 20.0),
    ];
    for (text, value) in cases {
        let ask = qa::match_text(text).unwrap_or_else(|| panic!("{text}"));
        let Args::Math(sum) = &ask.args else { panic!("{text}: {ask:?}") };
        assert!(ask.kind == Kind::Math && approx(sum.value.to_f64(), value, 1e-6), "{text}: {sum:?}");
    }
}

#[test]
fn test_no_sum_runs_away_or_runs_code() {
    assert_eq!(qa::arithmetic("2 ** 100000"), None); // too big to compute
    assert_eq!(qa::arithmetic("1 / 0"), None);
    assert_eq!(qa::arithmetic("__import__('os')"), None);
    assert_eq!(qa::arithmetic("what's 2 plus"), None);
}

#[test]
fn test_a_sum_too_big_for_a_float_is_left_to_the_model() {
    // 10**360 is an exact int; math.isfinite on it raised OverflowError and the message got no reply
    assert_eq!(qa::arithmetic("1000000 to the power of 60"), None);
    assert_eq!(qa::match_text("what is 1000000 to the power of 60"), None);
}

#[test]
fn test_the_conversions_are_right() {
    let cases = [
        ("convert 5 km to miles", 3.10686),
        ("72 fahrenheit in celsius", 22.2222),
        ("100 c to f", 212.0),
        ("how many ounces in a pound", 16.0),
        ("what's 3 cups in ml", 709.765),
        ("10 kg to pounds", 22.0462),
        ("6 feet to cm", 182.88),
        ("60 mph to kph", 96.5606),
    ];
    for (text, result) in cases {
        let ask = qa::match_text(text).unwrap_or_else(|| panic!("{text}"));
        let Args::Convert(c) = &ask.args else { panic!("{text}: {ask:?}") };
        assert!(ask.kind == Kind::Convert && approx(c.result, result, 1e-4), "{text}: {c:?}");
    }
}

#[test]
fn test_a_conversion_across_dimensions_is_not_one() {
    assert_eq!(qa::conversion("5 km to pounds"), None);
}

#[test]
fn test_countdowns_land_on_the_next_date() {
    let today = date(2026, 9, 26);
    assert_eq!(qa::holiday("christmas", today), Some(("christmas".into(), date(2026, 12, 25))));
    assert_eq!(qa::holiday("thanksgiving", today), Some(("thanksgiving".into(), date(2026, 11, 26)))); // 4th Thursday
    assert_eq!(qa::holiday("new year's", today), Some(("new year's".into(), date(2027, 1, 1))));
    assert_eq!(qa::holiday("halloween", date(2026, 11, 1)).unwrap().1, date(2027, 10, 31)); // passed: next year
    assert_eq!(
        qa::countdown_line("christmas", date(2026, 12, 25), today),
        "90 days until Christmas, on Friday, December 25."
    );
    assert_eq!(qa::countdown_line("halloween", date(2026, 10, 31), date(2026, 10, 31)), "Halloween is today!");
}

#[test]
fn test_the_words() {
    assert_eq!(qa::time_line(&now(), ""), "It's 4:05 PM.");
    assert_eq!(qa::date_line(&now()), "It's Saturday, September 26.");
    assert_eq!(qa::math_line("17 * 23", &Num::Int(391.into())), "17 * 23 is 391.");
    assert_eq!(
        qa::battery_line("Now drawing from 'AC Power'\n -InternalBattery-0 (id=1)\t100%; charged; 0:00").as_deref(),
        Some("The Mac is at 100%, fully charged.")
    );
    assert_eq!(
        qa::battery_line("Now drawing from 'Battery Power'\n -InternalBattery-0\t41%; discharging; 3:02").as_deref(),
        Some("The Mac is at 41%, on battery.")
    );
    assert_eq!(
        qa::air_line("Springfield", &json!({"current": {"us_aqi": 42}})).as_deref(),
        Some("The air in Springfield is good: US AQI 42.")
    );
}

fn forecast() -> Value {
    json!({
        "current": {"temperature_2m": 64.2, "apparent_temperature": 60.1, "weather_code": 2, "wind_speed_10m": 11.0},
        "daily": {"temperature_2m_min": [51.3, 49.0], "temperature_2m_max": [70.4, 66.6],
                  "precipitation_probability_max": [10, 60], "weather_code": [2, 61],
                  "sunrise": ["2026-09-26T07:03", "2026-09-27T07:04"], "sunset": ["2026-09-26T19:01", "2026-09-27T18:59"]},
    })
}

#[test]
fn test_weather_words() {
    assert_eq!(
        qa::weather_line("Springfield", &forecast(), 0).as_deref(),
        Some("In Springfield it's 64°F and partly cloudy, feels like 60°F, wind 11 mph. Today 51 to 70°F, 10% chance of rain.")
    );
    assert_eq!(
        qa::weather_line("Springfield", &forecast(), 1).as_deref(),
        Some("Tomorrow in Springfield: light rain, 49 to 67°F, 60% chance of rain.")
    );
    assert_eq!(
        qa::sun_line("Springfield", &forecast(), "sunset", 0).as_deref(),
        Some("Sunset in Springfield is at 7:01 PM today.")
    );
    assert_eq!(qa::weather_line("Springfield", &json!({"current": {}}), 0), None);
}

/// The faked Open-Meteo: records every call; the geocoder names the place in title case, in Tokyo's zone.
type Calls = Arc<Mutex<Vec<(String, Map<String, Value>)>>>;

#[derive(Clone, Default)]
struct Api {
    fail: bool,
    calls: Calls,
}

fn title(s: &str) -> String {
    s.split(' ')
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().chain(c.flat_map(char::to_lowercase)).collect()).unwrap_or_default()
        })
        .collect::<Vec<String>>()
        .join(" ")
}

impl Api {
    fn failing() -> Self {
        Api { fail: true, ..Default::default() }
    }

    fn getter(&self) -> qa::Getter {
        let api = self.clone();
        qa::getter(move |url: String, params: Map<String, Value>| {
            let api = api.clone();
            async move {
                api.calls.lock().push((url.clone(), params.clone()));
                if api.fail {
                    return Err(std::io::Error::other("offline").into());
                }
                if url == qa::GEOCODE_URL {
                    let name = params["name"].as_str().unwrap_or_default();
                    return Ok(json!({"results": [{"name": title(name), "latitude": 47.6, "longitude": -122.3,
                                                   "timezone": "Asia/Tokyo"}]}));
                }
                Ok(forecast())
            }
        })
    }

    fn urls(&self) -> Vec<String> {
        self.calls.lock().iter().map(|(u, _)| u.clone()).collect()
    }
}

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn quick(environ: HashMap<String, String>, api: &Api) -> QuickAnswers {
    QuickAnswers::new(Some(environ))
        .with_get(api.getter())
        .with_now(|_| now())
        .with_pmset(qa::pmset_fn(|| async { Ok("-InternalBattery-0\t77%; charging; 1:00".to_string()) }))
        // no phone: these tests are about the configured place (the phone's are in phone_location.rs)
        .with_phone(|| Ok(None))
}

fn ask(q: &QuickAnswers, text: &str) -> Option<String> {
    let a = q.match_text(text).unwrap_or_else(|| panic!("{text} is not a quick question"));
    rt().block_on(q.answer(&a))
}

#[test]
fn test_here_is_looked_up_once_and_answers_come_from_code() {
    let api = Api::default();
    let q = quick(env(&[("EVE_WEATHER_PLACE", "Springfield")]), &api);
    let a = ask(&q, "what's the weather").unwrap();
    let b = ask(&q, "when is sunset").unwrap();
    assert!(a.starts_with("In Springfield it's 64°F"), "{a}");
    assert_eq!(b, "Sunset in Springfield is at 7:01 PM today.");
    assert_eq!(api.urls().iter().filter(|u| *u == qa::GEOCODE_URL).count(), 1);
}

#[test]
fn test_without_a_home_place_weather_is_the_models() {
    let q = quick(env(&[]), &Api::default());
    assert_eq!(q.match_text("what's the weather"), None);
    assert!(q.match_text("what's the weather in Paris").is_some()); // a named place needs no home
    assert!(q.match_text("what time is it").is_some());
}

#[test]
fn test_a_failed_source_hands_the_question_back_to_the_model() {
    let q = quick(env(&[("EVE_WEATHER_PLACE", "Springfield")]), &Api::failing());
    assert_eq!(ask(&q, "what's the weather"), None);
    assert_eq!(ask(&q, "what time is it").as_deref(), Some("It's 4:05 PM.")); // the clock never fails
}

#[test]
fn test_time_elsewhere_uses_the_places_zone() {
    let q = quick(env(&[]), &Api::default());
    let line = ask(&q, "what time is it in tokyo");
    assert!(line.as_deref().is_some_and(|l| l.ends_with("in Tokyo.")), "{line:?}");
}

#[test]
fn test_the_battery_is_the_macs() {
    let q = quick(env(&[]), &Api::default());
    assert_eq!(ask(&q, "what's my battery").as_deref(), Some("The Mac is at 77%, charging."));
}

// ---- beyond the Python file: the edges Python's code takes ---------------------------------------------------------

#[test]
fn the_weather_params_are_pythons() {
    let api = Api::default();
    let q = quick(env(&[("EVE_WEATHER_LAT", "10"), ("EVE_WEATHER_LON", "20")]), &api);
    assert!(ask(&q, "what's the weather").unwrap().starts_with("In your area it's 64°F"));
    let calls = api.calls.lock().clone();
    let (url, params) = calls.last().unwrap();
    assert_eq!(url, qa::FORECAST_URL);
    assert_eq!(
        Value::Object(params.clone()),
        json!({"latitude": 10.0, "longitude": 20.0,
               "current": "temperature_2m,apparent_temperature,weather_code,wind_speed_10m",
               "daily": "temperature_2m_max,temperature_2m_min,precipitation_probability_max,weather_code",
               "temperature_unit": "fahrenheit", "wind_speed_unit": "mph", "timezone": "auto", "forecast_days": 2})
    );
    assert_eq!(params.keys().next().map(String::as_str), Some("latitude"));
}

#[test]
fn bad_coordinates_and_a_bad_zone_are_ignored() {
    let q = quick(env(&[("EVE_WEATHER_LAT", "north"), ("EVE_WEATHER_LON", "20"), ("EVE_TIMEZONE", "Mars/Base")]), &Api::default());
    assert!(q.home().is_none() && q.tz.is_none());
    assert_eq!(q.match_text("what's the weather"), None);
    let q = quick(env(&[("EVE_TIMEZONE", "america/los_angeles")]), &Api::default());
    assert_eq!(q.tz, Some(chrono_tz::America::Los_Angeles)); // the Mac's file system does not mind the case
}

#[test]
fn a_place_the_geocoder_does_not_know_is_the_models_and_is_remembered() {
    let calls: Arc<Mutex<usize>> = Arc::default();
    let seen = calls.clone();
    let q = QuickAnswers::new(Some(env(&[])))
        .with_now(|_| now())
        .with_phone(|| Ok(None))
        .with_get(qa::getter(move |_url, _params| {
            let seen = seen.clone();
            async move {
                *seen.lock() += 1;
                Ok(json!({"results": []}))
            }
        }));
    assert_eq!(ask(&q, "what's the weather in atlantis"), None);
    assert_eq!(ask(&q, "what's the weather in Atlantis"), None);
    assert_eq!(*calls.lock(), 1);
}

#[test]
fn the_tomorrow_and_air_answers() {
    let q = quick(env(&[("EVE_WEATHER_PLACE", "Springfield")]), &Api::default());
    assert_eq!(
        ask(&q, "weather tomorrow").as_deref(),
        Some("Tomorrow in Springfield: light rain, 49 to 67°F, 60% chance of rain.")
    );
    assert_eq!(ask(&q, "what time is sunrise tomorrow").as_deref(), Some("Sunrise in Springfield is at 7:04 AM tomorrow."));
    // the forecast has no us_aqi: the air question goes back to the model
    assert_eq!(ask(&q, "what's the air quality"), None);
    assert_eq!(ask(&q, "how many days until christmas").as_deref(), Some("90 days until Christmas, on Friday, December 25."));
    assert_eq!(ask(&q, "convert 5 km to miles").as_deref(), Some("5 km is 3.1069 miles."));
    assert_eq!(ask(&q, "what's 17 times 23").as_deref(), Some("17 times 23 is 391.")); // said as asked
    assert_eq!(ask(&q, "15 percent of 80").as_deref(), Some("15 percent of 80 is 12."));
}

#[test]
fn the_words_take_pythons_types() {
    // strings and bools are numbers to float(); a missing rain chance is no chance; a code past the table no sky
    let data = json!({"current": {"temperature_2m": "64.6", "apparent_temperature": 64, "weather_code": 999,
                                  "wind_speed_10m": true},
                      "daily": {"temperature_2m_min": [50.5], "temperature_2m_max": [71.5]}});
    assert_eq!(
        qa::weather_line("X", &data, 0).as_deref(),
        Some("In X it's 65°F, wind 1 mph. Today 50 to 72°F.") // round() ties to even
    );
    assert_eq!(qa::air_line("X", &json!({"current": {"us_aqi": 10_001}})), None);
    assert_eq!(qa::air_line("X", &json!({"current": {"us_aqi": 151}})).as_deref(), Some("The air in X is unhealthy: US AQI 151."));
    assert_eq!(qa::battery_line("-InternalBattery-0\t80%; discharging; AC Power").as_deref(), Some("The Mac is at 80%, plugged in."));
    assert_eq!(qa::battery_line("nothing"), None);
}
