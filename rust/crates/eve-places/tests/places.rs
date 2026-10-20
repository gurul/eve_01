//! test_places.py: the places history, habits, digests and things, with a lent clock and synthetic weeks (no
//! network).

use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use chrono_tz::America::Los_Angeles as PT;
use eve_places::phone_location;
use eve_places::places::{self, Places, Visit};
use eve_places::{py, Env};
use serde_json::{json, Map, Value};

// made-up points in Chicago, none of them anyone's
const HOME: (f64, f64) = (41.8800, -87.6300);
const WORK: (f64, f64) = (41.8890, -87.6350); // about 1 km north
const GYM: (f64, f64) = (41.8700, -87.6400);
const MARKET: (f64, f64) = (41.8950, -87.6200);

/// A Thursday noon.
fn now() -> DateTime<Utc> {
    PT.with_ymd_and_hms(2026, 10, 22, 12, 0, 0).unwrap().with_timezone(&Utc)
}

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, 22).unwrap()
}

fn t(d: NaiveDate, hh: u32, mm: u32) -> DateTime<Utc> {
    PT.with_ymd_and_hms(d.year(), d.month(), d.day(), hh, mm, 0).unwrap().with_timezone(&Utc)
}

fn env(tmp: &Path) -> Env {
    Env::from([
        ("EVE_TIMEZONE".to_string(), "America/Los_Angeles".to_string()),
        ("EVE_PLACES_FILE".to_string(), tmp.join("places.db").display().to_string()),
        ("EVE_PHONE_LOCATION_FILE".to_string(), tmp.join("phone.json").display().to_string()),
    ])
}

fn store_at(tmp: &Path, at: DateTime<Utc>) -> Places {
    Places::new(None, Some(env(tmp)), Some(Arc::new(move || at)))
}

fn store(tmp: &Path) -> Places {
    store_at(tmp, now())
}

/// As the phone sends it: the arrival, then the departure with the same arrival.
fn visit_full(s: &Places, at: (f64, f64), a: DateTime<Utc>, b: Option<DateTime<Utc>>, poi: Option<&str>, category: Option<&str>) {
    let v = |departed| Visit {
        lat: at.0,
        lon: at.1,
        accuracy_m: 20.0,
        arrived: Some(a),
        departed,
        place: None,
        poi: poi.map(str::to_string),
        category: category.map(str::to_string),
    };
    s.record_visit(&v(None)).unwrap();
    if b.is_some() {
        s.record_visit(&v(b)).unwrap();
    }
}

fn visit(s: &Places, at: (f64, f64), a: DateTime<Utc>, b: Option<DateTime<Utc>>) {
    visit_full(s, at, a, b, None, None);
}

/// Home nights, work 09:00-17:30 on weekdays, the gym Mon/Wed/Fri 18:00-19:00, a market one Saturday.
fn four_weeks(s: &Places, skip_gym_on: Option<NaiveDate>) {
    let first = today() - Duration::days(28);
    let mut home_from = t(first - Duration::days(1), 19, 15);
    let first_saturday = first + Duration::days(((5 - first.weekday().num_days_from_monday() as i64) % 7 + 7) % 7);
    let mut d = first;
    while d < today() {
        let wd = d.weekday().num_days_from_monday();
        if wd < 5 {
            visit(s, HOME, home_from, Some(t(d, 8, 30)));
            visit(s, WORK, t(d, 9, 0), Some(t(d, 17, 30)));
            if [0, 2, 4].contains(&wd) && Some(d) != skip_gym_on {
                visit_full(s, GYM, t(d, 18, 0), Some(t(d, 19, 0)), Some("Equinox"), Some("fitnessCenter"));
                home_from = t(d, 19, 15);
            } else {
                home_from = t(d, 17, 50);
            }
        } else if wd == 5 && d == first_saturday {
            visit(s, HOME, home_from, Some(t(d, 10, 40)));
            visit_full(s, MARKET, t(d, 11, 0), Some(t(d, 12, 0)), Some("Green City Market"), None);
            home_from = t(d, 12, 20);
        }
        d += Duration::days(1);
    }
    visit(s, HOME, home_from, Some(t(today(), 8, 30)));
    visit(s, WORK, t(today(), 9, 0), None); // at work now
}

fn share_fix(e: &Env, at: (f64, f64), place: &str) {
    let fix = phone_location::parse(
        &json!({"lat": at.0, "lon": at.1, "accuracy_m": 10.0, "at": py::isoformat_utc(&Utc::now()), "place": place,
                "speed_mps": null}),
        None,
    )
    .unwrap();
    let path = Path::new(&e["EVE_PHONE_LOCATION_FILE"]).to_path_buf();
    let _ = std::fs::remove_file(&path);
    assert!(phone_location::store(&fix, Some(&path)).unwrap());
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or_default()
}

fn places_of(rows: &[Value]) -> Vec<String> {
    rows.iter().map(|x| s(&x["place"]).to_string()).collect()
}

fn args(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

// ---- visits and places -------------------------------------------------------------------------------------------

#[test]
fn test_an_arrival_and_its_departure_are_one_visit_at_a_named_place() {
    let tmp = tempfile::tempdir().unwrap();
    let st = store(tmp.path());
    let d = today();
    visit_full(&st, GYM, t(d, 7, 0), Some(t(d, 8, 5)), Some("Equinox"), Some("fitnessCenter"));
    let tl = st.timeline(d, d).unwrap();
    assert_eq!(tl.len(), 1);
    let v = &tl[0];
    assert!(v["place"] == "Equinox" && v["kind"] == "fitness center");
    assert!(s(&v["arrived"]).ends_with("07:00") && s(&v["left"]).ends_with("08:05") && v["stayed"] == "1 h 5 min");
    assert!(!v.contains_key("approximate") && !v.contains_key("lat") && !v.contains_key("lon")); // words, not coordinates
}

#[test]
fn test_nearby_visits_share_a_place_and_a_far_one_starts_another() {
    let tmp = tempfile::tempdir().unwrap();
    let st = store(tmp.path());
    let d = today();
    visit_full(&st, GYM, t(d, 6, 0), Some(t(d, 7, 0)), Some("Equinox"), None);
    visit_full(&st, (GYM.0 + 0.0018, GYM.1), t(d, 8, 0), Some(t(d, 9, 0)), Some("Equinox"), None); // 200 m off, same POI
    visit(&st, (GYM.0 + 0.0006, GYM.1), t(d, 10, 0), Some(t(d, 11, 0))); // 70 m off, no name
    visit(&st, MARKET, t(d, 11, 30), Some(t(d, 11, 45)));
    let sum = st.summary().unwrap();
    assert!(sum["places"] == 2 && sum["visits"] == 4);
}

#[test]
fn test_a_stay_is_read_from_fixes_and_an_ios_visit_replaces_it() {
    let tmp = tempfile::tempdir().unwrap();
    let st = store(tmp.path());
    let d = today();
    let fix = |at: (f64, f64), when: DateTime<Utc>| {
        st.record_fix(&phone_location::Fix::new(at.0, at.1, 15.0, when, Some("Main St"))).unwrap()
    };
    assert!(fix(MARKET, t(d, 9, 0)).is_none());
    assert!(fix((MARKET.0 + 0.0003, MARKET.1), t(d, 9, 5)).is_none());
    assert!(fix(MARKET, t(d, 9, 20)).is_none());
    assert!(fix(HOME, t(d, 9, 40)).is_some()); // 1.7 km away: the stay ended before this fix
    let tl = st.timeline(d, d).unwrap();
    assert_eq!(tl.len(), 1);
    assert!(tl[0]["approximate"] == true && s(&tl[0]["arrived"]).ends_with("09:00"));
    let left = s(&tl[0]["left"]);
    let n = left.len();
    let minute: i64 = left[n - 5..n - 3].parse::<i64>().unwrap() * 60 + left[n - 2..].parse::<i64>().unwrap();
    assert!((9 * 60 + 20..9 * 60 + 40).contains(&minute)); // the far fix minus the travel time
    visit_full(&st, MARKET, t(d, 8, 58), Some(t(d, 9, 33)), Some("Green City Market"), None);
    let tl = st.timeline(d, d).unwrap();
    assert_eq!(tl.len(), 1);
    assert!(tl[0]["place"] == "Green City Market" && !tl[0].contains_key("approximate"));
}

#[test]
fn test_a_short_stop_and_a_vague_fix_are_not_visits() {
    let tmp = tempfile::tempdir().unwrap();
    let st = store(tmp.path());
    let d = today();
    let f = |at: (f64, f64), acc: f64, when| phone_location::Fix::new(at.0, at.1, acc, when, None);
    st.record_fix(&f(MARKET, 15.0, t(d, 9, 0))).unwrap();
    st.record_fix(&f(MARKET, 5000.0, t(d, 9, 30))).unwrap(); // a cell-tower guess says nothing
    assert!(st.record_fix(&f(HOME, 15.0, t(d, 9, 6))).unwrap().is_none()); // six minutes: passing through
    assert!(st.timeline(d, d).unwrap().is_empty());
}

#[test]
fn test_a_body_that_is_not_a_visit_is_refused() {
    let cases = [
        json!([]),
        json!({"lat": 0, "lon": 0, "accuracy_m": 5, "arrived": null, "departed": null}),
        json!({"lat": 91, "lon": 0, "accuracy_m": 5, "arrived": "2026-10-18T09:00:00Z", "departed": null}),
        json!({"lat": 0, "lon": 0, "accuracy_m": 5, "arrived": "2026-10-18T09:00:00", "departed": null}),
        json!({"lat": 0, "lon": 0, "accuracy_m": 5, "arrived": "2026-10-18T10:00:00Z", "departed": "2026-10-18T09:00:00Z"}),
        json!({"lat": 0, "lon": 0, "accuracy_m": 5, "arrived": "2099-01-01T00:00:00Z", "departed": null}),
        json!({"lat": 0, "lon": 0, "accuracy_m": 5, "arrived": "2026-10-18T09:00:00Z", "departed": null, "poi": 5}),
    ];
    let want = [
        "the body must be a JSON object",
        "a visit needs arrived or departed",
        "lat must be from -90 to 90",
        "arrived must be an ISO 8601 time with a zone, or null",
        "departed is before arrived",
        "a visit's time is in the future",
        "place, poi and category: must be a string or null",
    ];
    for (body, reason) in cases.iter().zip(want) {
        assert_eq!(places::parse_visit(body, None).unwrap_err().0, reason, "{body}");
    }
}

#[test]
fn test_a_visit_body_parses_and_drops_an_odd_category() {
    let v = places::parse_visit(
        &json!({"lat": 1, "lon": 2, "accuracy_m": 30, "arrived": "2026-10-18T09:00:00Z", "departed": null,
                "place": " 1st  Ave ", "poi": "Crux", "category": "rock climbing!"}),
        None,
    )
    .unwrap();
    assert_eq!(
        (v.place.as_deref(), v.poi.as_deref(), v.category.as_deref(), v.departed),
        (Some("1st Ave"), Some("Crux"), None, None)
    );
}

// ---- home, work and habits ---------------------------------------------------------------------------------------

#[test]
fn test_home_and_work_are_inferred_and_named() {
    let tmp = tempfile::tempdir().unwrap();
    let st = store(tmp.path());
    four_weeks(&st, None);
    let labels = st.infer_labels().unwrap();
    assert!(labels.home.is_some() && labels.work.is_some() && labels.home != labels.work);
    let names: Vec<Value> = st.timeline(today(), today()).unwrap().into_iter().map(Value::Object).collect();
    assert_eq!(places_of(&names), ["Home", "Work"]);
}

#[test]
fn test_habits_are_the_usual_days_times_and_stays() {
    let tmp = tempfile::tempdir().unwrap();
    let st = store(tmp.path());
    four_weeks(&st, None);
    let out = st.patterns().unwrap();
    let list = out["places"].as_array().unwrap();
    let by = |name: &str| list.iter().find(|p| p["place"] == name).cloned();
    let gym = by("Equinox").unwrap();
    assert_eq!(gym["usual_days"], "Mon, Wed and Fri");
    assert!(gym["usual_arrival"] == "18:00" && gym["usual_stay"] == "1 h");
    let work = by("Work").unwrap();
    assert!(work["usual_days"] == "weekdays" && work["usual_arrival"] == "09:00");
    if let Some(market) = by("Green City Market") {
        assert!(market.get("usual_days").is_none());
    }
    let routine: Vec<&str> = out["routine"].as_array().unwrap().iter().map(s).collect();
    assert!(routine.iter().any(|l| l.contains("leave home around 08:30")));
    assert!(routine.iter().any(|l| l.contains("home by")));
}

#[test]
fn test_a_day_digest_says_the_day_in_order_and_what_was_missed() {
    let wednesday = today() - Duration::days(1);
    let tmp = tempfile::tempdir().unwrap();
    let st = store(tmp.path());
    four_weeks(&st, Some(wednesday));
    let out = st.digest(wednesday).unwrap();
    let stops = out["stops"].as_array().unwrap();
    assert_eq!(places_of(stops), ["Home", "Work", "Home"]);
    assert!(stops[1]["from"] == "09:00" && stops[1]["to"] == "17:30");
    assert!(out["at_work"] == "8 h 30 min" && out.contains_key("at_home"));
    assert_eq!(out["missed"], json!(["Equinox (usually Mon, Wed and Fri)"]));
    assert!(s(&out["text"]).starts_with("Wednesday 2026-10-21: Home 00:00-08:30"), "{}", out["text"]);
    let monday = wednesday - Duration::days(2);
    let mon = st.digest(monday).unwrap();
    assert!(!mon.contains_key("missed") && places_of(mon["stops"].as_array().unwrap()).contains(&"Equinox".to_string()));
}

#[test]
fn test_today_s_digest_ends_now_and_a_week_digest_ranks_places() {
    let tmp = tempfile::tempdir().unwrap();
    let st = store(tmp.path());
    four_weeks(&st, None);
    let today_d = st.digest(today()).unwrap();
    assert_eq!(
        today_d["stops"].as_array().unwrap().last().unwrap(),
        &json!({"place": "Work", "from": "09:00", "to": "now", "stayed": "3 h"})
    );
    assert!(!today_d.contains_key("missed"));
    let week = st.week(today() - Duration::days(7)).unwrap();
    let ps = week["places"].as_array().unwrap();
    assert!(ps[0]["place"] == "Home" && ps[1]["place"] == "Work");
    assert!(ps[1]["days"] == 5 && s(&week["text"]).starts_with("Week of 2026-10-12: Home"));
    assert_eq!(ps[0]["days"], 7); // a night at home touches two days; all 7 are counted
}

#[test]
fn test_new_places_show_in_their_day() {
    let tmp = tempfile::tempdir().unwrap();
    let st = store(tmp.path());
    let d = today();
    visit_full(&st, MARKET, t(d, 9, 0), Some(t(d, 10, 0)), Some("Green City Market"), None);
    assert_eq!(st.digest(d).unwrap()["new_places"], json!(["Green City Market"]));
    assert!(!st.digest(d - Duration::days(1)).unwrap().contains_key("new_places"));
}

// ---- naming, things, here ----------------------------------------------------------------------------------------

#[test]
fn test_the_owner_names_where_they_are_and_renames_it() {
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path());
    let st = Places::new(None, Some(e.clone()), None);
    assert_eq!(st.name_place("Mom's", "").unwrap()["reason"], places::NO_HERE);
    share_fix(&e, MARKET, "a street");
    assert_eq!(
        Value::Object(st.name_place("Mom's", "").unwrap()),
        json!({"ok": true, "id": 1, "place": "Mom's", "address": "a street"})
    );
    assert_eq!(st.name_place("Mom and Dad's", "mom's").unwrap()["place"], "Mom and Dad's");
    assert_eq!(st.name_place("x", "nowhere I know").unwrap()["ok"], false);
}

#[test]
fn test_things_are_kept_where_the_owner_is_and_found_with_how_far() {
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path());
    let st = Places::new(None, Some(e.clone()), None);
    share_fix(&e, MARKET, "a street");
    assert_eq!(st.remember_thing("my car", "level 3, row F", "").unwrap()["ok"], true);
    let found = st.find_thing("the car").unwrap();
    assert!(found["ok"] == true && found["thing"] == "my car" && found["note"] == "level 3, row F");
    assert!(found["from_here"] == "right here" && s(&found["map"]).starts_with("https://maps.apple.com/?ll="));
    share_fix(&e, HOME, "a street");
    assert_eq!(st.find_thing("car").unwrap()["from_here"], "about 1.9 km"); // 1.67 km north, 0.83 km east
    let listed = st.list_things().unwrap();
    let names: Vec<&str> = listed["things"].as_array().unwrap().iter().map(|x| s(&x["thing"])).collect();
    assert_eq!(names, ["my car"]);
    let miss = st.find_thing("bike").unwrap();
    assert!(miss["ok"] == false && miss["things"] == json!(["my car"]));
    assert_eq!(st.forget_thing("car").unwrap()["ok"], true);
    assert_eq!(st.list_things().unwrap()["things"], json!([]));
}

#[test]
fn test_a_thing_at_a_known_place_needs_no_fix() {
    let tmp = tempfile::tempdir().unwrap();
    let st = store(tmp.path());
    visit_full(&st, MARKET, t(today(), 9, 0), Some(t(today(), 10, 0)), Some("Sam's"), None);
    let out = st.remember_thing("jacket", "", "Sam's").unwrap();
    assert_eq!(Value::Object(out), json!({"ok": true, "thing": "jacket", "place": "Sam's"}));
    assert_eq!(st.find_thing("jacket").unwrap()["place"], "Sam's");
}

#[test]
fn test_where_the_owner_is_now_is_said_by_name_in_the_system_context() {
    let tmp = tempfile::tempdir().unwrap();
    let mut e = env(tmp.path());
    let st = Places::new(None, Some(e.clone()), None);
    assert_eq!(places::here_line(Some(&e)), ""); // no fix, nothing said
    let arrived = Utc::now() - Duration::minutes(40);
    visit_full(&st, GYM, arrived, None, Some("Equinox"), Some("fitnessCenter"));
    share_fix(&e, GYM, "a street");
    let since = py::trunc_micros(arrived).with_timezone(&PT).format("%H:%M").to_string();
    let line = format!("Where the owner is now, from their phone: Equinox (fitness center), since {since}.");
    assert_eq!(places::here_line(Some(&e)), line);
    let reading = phone_location::reading(Some(&e), None);
    assert_eq!(reading["at"], json!({"place": "Equinox", "kind": "fitness center", "since": since}));
    e.insert("EVE_PLACES".into(), "0".into());
    assert_eq!(places::here_line(Some(&e)), "");
    assert!(!phone_location::reading(Some(&e), None).contains_key("at"));
}

// ---- tools -------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn test_the_tools_ride_with_phone_location_once_the_phone_has_shared() {
    let tmp = tempfile::tempdir().unwrap();
    let mut e = env(tmp.path());
    assert_eq!(phone_location::PLACES_TOOL_NAMES, places::TOOL_NAMES);
    assert!(phone_location::tools(Some(&e)).is_empty());
    share_fix(&e, MARKET, "a street");
    let names: Vec<String> = phone_location::tools(Some(&e)).iter().map(|x| s(&x["name"]).to_string()).collect();
    let mut want = vec![phone_location::TOOL_NAME.to_string()];
    want.extend(places::TOOL_NAMES.iter().map(|n| n.to_string()));
    assert_eq!(names, want);
    for tool in places::TOOLS.iter() {
        let params = &tool["parameters"];
        let required: std::collections::BTreeSet<&str> = params["required"].as_array().unwrap().iter().map(s).collect();
        let props: std::collections::BTreeSet<&str> = params["properties"].as_object().unwrap().keys().map(String::as_str).collect();
        assert!(tool["strict"] == true && required == props);
    }
    e.insert("EVE_PLACES".into(), "0".into());
    let names: Vec<String> = phone_location::tools(Some(&e)).iter().map(|x| s(&x["name"]).to_string()).collect();
    assert_eq!(names, [phone_location::TOOL_NAME]);
    let out = phone_location::handle(places::RECALL, &args(json!({"what": "day", "start_date": "", "end_date": ""})), Some(&e)).await;
    assert_eq!(Value::Object(out), json!({"ok": false, "reason": places::OFF_REASON}));
}

#[test]
fn test_the_recall_tool_answers_each_kind_and_refuses_bad_dates() {
    let tmp = tempfile::tempdir().unwrap();
    let st = store(tmp.path());
    four_weeks(&st, None);
    let e = env(tmp.path());
    let recall = |what: &str, start: &str, end: &str| {
        places::call(places::RECALL, &args(json!({"what": what, "start_date": start, "end_date": end})), Some(&st), Some(&e))
    };
    assert_eq!(recall("day", "", "")["day"], "Thursday 2026-10-22");
    assert_eq!(recall("week", "2026-10-14", "")["week"], "2026-10-12 to 2026-10-18");
    assert_eq!(recall("timeline", "2026-10-19", "2026-10-21")["visits"].as_array().unwrap().len(), 9);
    assert!(!recall("habits", "", "")["places"].as_array().unwrap().is_empty());
    assert_eq!(recall("places", "", "")["places"][0]["place"], "Home");
    assert_eq!(recall("timeline", "2026-10-21", "2026-10-19")["ok"], false);
    assert_eq!(recall("day", "21 Oct", "")["reason"], "dates are YYYY-MM-DD");
    assert_eq!(recall("day", "2026-13-01", "")["reason"], "month must be in 1..12");
    assert_eq!(recall("nonsense", "", "")["reason"], "what is day, week, timeline, habits or places");
    let thing = places::call(places::THING, &args(json!({"action": "list", "thing": "", "note": "", "place": ""})), Some(&st), Some(&e));
    assert_eq!(Value::Object(thing), json!({"ok": true, "things": []}));
}

// ---- keeping and forgetting --------------------------------------------------------------------------------------

#[test]
fn test_the_file_is_owner_only_and_old_evidence_is_pruned() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let st = store(tmp.path());
    let old = now() - Duration::days(places::FIX_DAYS + 1);
    st.record_fix(&phone_location::Fix::new(HOME.0, HOME.1, 10.0, old, None)).unwrap();
    st.record_fix(&phone_location::Fix::new(HOME.0, HOME.1, 10.0, now() - Duration::hours(1), None)).unwrap();
    assert_eq!(std::fs::metadata(&st.path).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(st.prune().unwrap()["fixes"], 1);
}

#[test]
fn test_forget_wipes_everything_with_the_latest_fix() {
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path());
    let st = Places::new(None, Some(e.clone()), None);
    share_fix(&e, MARKET, "a street");
    visit(&st, MARKET, Utc::now() - Duration::hours(2), Some(Utc::now()));
    st.remember_thing("bike", "", "").unwrap();
    let forgot = st.forget().unwrap();
    assert!(forgot["places"] >= 1 && forgot["visits"] == 1 && forgot["things"] == 1);
    assert_eq!(Value::Object(st.summary().unwrap()), json!({"places": 0, "visits": 0, "things": 0, "since": null}));
    assert!(!Path::new(&e["EVE_PHONE_LOCATION_FILE"]).exists());
}

// ---- the store reads what Python wrote ---------------------------------------------------------------------------

#[test]
fn test_the_stay_is_kept_as_python_json_in_meta() {
    let tmp = tempfile::tempdir().unwrap();
    let st = store(tmp.path());
    let at = t(today(), 9, 0);
    st.record_fix(&phone_location::Fix::new(MARKET.0, MARKET.1, 15.0, at, Some("Main St"))).unwrap();
    let con = rusqlite::Connection::open(&st.path).unwrap();
    let raw: String = con.query_row("select value from meta where key = 'stay'", [], |r| r.get(0)).unwrap();
    let ts = py::float_repr(py::timestamp(&at));
    assert_eq!(
        raw,
        format!(r#"{{"lat": 41.895, "lon": -87.62, "acc": 15.0, "arrived": {ts}, "last": {ts}, "n": 1, "place": "Main St"}}"#)
    );
}

#[test]
fn test_a_day_in_a_dst_change_is_bounded_like_python() {
    // 2026-11-01 is the fall-back day in Los Angeles: 25 hours long
    let tmp = tempfile::tempdir().unwrap();
    let st = store_at(tmp.path(), t(NaiveDate::from_ymd_opt(2026, 11, 3).unwrap(), 12, 0));
    let d = NaiveDate::from_ymd_opt(2026, 11, 1).unwrap();
    visit(&st, HOME, t(d - Duration::days(1), 20, 0), Some(t(d + Duration::days(1), 7, 0)));
    let out = st.digest(d).unwrap();
    assert_eq!(out["stops"][0]["stayed"], "25 h");
    assert!(places::zone(Some(&env(tmp.path()))) == places::Zone::Named(chrono_tz::America::Los_Angeles));
}
