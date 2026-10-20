//! Port of `bridge/tests/test_follow.py` (and the follower part of `test_desk_call.py`).
//!
//! A fake clock and a recording sender; faces are vision.FaceResult-shaped (bx, by, size, conf). One degree
//! of yaw is about 3 units of bx (a 66° lens: 100 units = 33°).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use futures::executor::block_on;
use parking_lot::Mutex;
use serde_json::{Map, Value};

use super::*;

/// The box a face at (true_yaw, true_pitch) makes in a frame taken at the given pose.
fn face_at(true_yaw: f64, true_pitch: f64, pose_yaw: f64, pose_pitch: f64) -> Face {
    face_at_sized(true_yaw, true_pitch, pose_yaw, pose_pitch, 22, 90)
}

fn face_at_sized(true_yaw: f64, true_pitch: f64, pose_yaw: f64, pose_pitch: f64, size: i64, conf: i64) -> Face {
    let bx = (true_yaw - pose_yaw) / 33.0 * 100.0;
    let by = -(true_pitch - pose_pitch) / 24.75 * 100.0;
    Face {
        bx: bx.clamp(-100.0, 100.0).round_ties_even() as i64,
        by: by.clamp(-100.0, 100.0).round_ties_even() as i64,
        size,
        conf,
    }
}

struct BenchState {
    t: f64,
    sent: Vec<Value>,
    events: Vec<(String, Map<String, Value>)>,
    ok: bool,
    pose: (f64, f64), // where the head is: every accepted look moves it there at once
}

struct Bench {
    s: Arc<Mutex<BenchState>>,
    f: Arc<SpeakerFollower>,
}

type Where = Option<(f64, f64)>;

impl Bench {
    fn new() -> Self {
        Self::with(|_| {})
    }

    fn with(kw: impl FnOnce(&mut FollowerOptions)) -> Self {
        let s = Arc::new(Mutex::new(BenchState { t: 100.0, sent: vec![], events: vec![], ok: true, pose: (0.0, 45.0) }));
        let (s1, s2, s3) = (s.clone(), s.clone(), s.clone());
        let send: Sender = Arc::new(move |cmd: Value| {
            let s = s1.clone();
            Box::pin(async move {
                let mut st = s.lock();
                st.sent.push(cmd.clone());
                if st.ok && cmd["cmd"] == "look" {
                    st.pose = (cmd["yaw"].as_f64().unwrap(), cmd["pitch"].as_f64().unwrap());
                }
                st.ok
            })
        });
        let mut opts = FollowerOptions {
            clock: Arc::new(move || s2.lock().t),
            publish: Some(Arc::new(move |topic: &str, msg: Map<String, Value>| {
                s3.lock().events.push((topic.to_string(), msg));
                Ok(())
            })),
            ..FollowerOptions::default()
        };
        kw(&mut opts);
        Bench { s, f: Arc::new(SpeakerFollower::with(send, opts)) }
    }

    fn t(&self) -> f64 {
        self.s.lock().t
    }
    fn set_t(&self, t: f64) {
        self.s.lock().t = t;
    }
    fn pose(&self) -> (f64, f64) {
        self.s.lock().pose
    }
    fn set_pose(&self, pose: (f64, f64)) {
        self.s.lock().pose = pose;
    }
    fn set_ok(&self, ok: bool) {
        self.s.lock().ok = ok;
    }
    fn sent(&self) -> Vec<Value> {
        self.s.lock().sent.clone()
    }
    fn n_sent(&self) -> usize {
        self.s.lock().sent.len()
    }

    /// One frame: a person at absolute `where` (or None for nobody), seen from the current pose.
    fn see_with(&self, where_: Where, dt: f64, extra: &[Face]) {
        let pose = {
            let mut st = self.s.lock();
            st.t += dt;
            st.pose
        };
        let mut faces: Vec<Face> = extra.to_vec();
        if let Some(w) = where_ {
            if (w.0 - pose.0).abs() <= 33.0 && (w.1 - pose.1).abs() <= 24.0 {
                faces.push(face_at(w.0, w.1, pose.0, pose.1));
            }
        }
        block_on(self.f.on_faces(&faces, Some(pose.0), Some(pose.1)));
    }

    fn see(&self, where_: Where, dt: f64) {
        self.see_with(where_, dt, &[])
    }

    fn watch_fn(&self, where_: impl Fn(f64) -> Where, secs: f64, dt: f64) {
        for _ in 0..((secs / dt) as i64) {
            self.see(where_(self.t()), dt);
        }
    }

    fn watch(&self, where_: Where, secs: f64) {
        self.watch_fn(|_| where_, secs, 0.25)
    }

    fn watch_dt(&self, where_: Where, secs: f64, dt: f64) {
        self.watch_fn(|_| where_, secs, dt)
    }

    fn faces(&self, faces: &[Face], yaw: f64, pitch: f64) {
        block_on(self.f.on_faces(faces, Some(yaw), Some(pitch)));
    }

    fn phase(&self, name: &str) {
        block_on(self.f.on_phase(name));
    }

    fn looks(&self) -> Vec<(i64, i64, i64)> {
        self.sent()
            .iter()
            .filter(|c| c["cmd"] == "look")
            .map(|c| (c["yaw"].as_i64().unwrap(), c["pitch"].as_i64().unwrap(), c["hold"].as_i64().unwrap()))
            .collect()
    }

    fn states(&self) -> Vec<String> {
        self.s
            .lock()
            .events
            .iter()
            .map(|(_t, m)| {
                let mut s = m["state"].as_str().unwrap().to_string();
                if let Some(how) = m.get("how") {
                    s.push(':');
                    s.push_str(how.as_str().unwrap());
                }
                s
            })
            .collect()
    }

    fn track(&self) -> Track {
        self.f.track().expect("a track")
    }
}

const CHAIR: (f64, f64) = (-23.0, 38.0); // where the owner really sat on 2026-09-21: to eve's left, a little below level
const C: Where = Some(CHAIR);

fn whys(places: &[Place]) -> Vec<String> {
    places.iter().map(|(_, _, w)| w.clone()).collect()
}

fn dedupe(v: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in v {
        if !out.contains(&w) {
            out.push(w);
        }
    }
    out
}

fn wall_of(clock: &Arc<Mutex<f64>>) -> Wall {
    let c = clock.clone();
    Arc::new(move || *c.lock())
}

fn read_json(path: &std::path::Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn test_it_only_follows_while_eve_should_be_looking_at_its_person() {
    for phase in ["thinking", "working", "idle", "done", "error"] {
        let b = Bench::new();
        b.phase(phase);
        b.watch(C, 4.0);
        assert!(b.sent().is_empty(), "{phase}");
    }
    for phase in FOLLOW_PHASES {
        let b = Bench::new();
        b.phase(phase);
        b.watch(C, 6.0);
        assert!(b.f.holding() && (b.pose().0 - CHAIR.0).abs() <= DEADBAND_YAW, "{phase} {:?}", b.pose());
    }
}

#[test]
fn test_it_converges_on_a_still_person_and_then_sits_still() {
    let b = Bench::new();
    b.phase("listening");
    b.watch(C, 8.0);
    assert!((b.pose().0 - CHAIR.0).abs() <= DEADBAND_YAW && (b.pose().1 - CHAIR.1).abs() <= DEADBAND_PITCH);
    let moves = b.f.moves();
    assert!(moves <= 3); // it walked there in steps, it did not hunt
    b.watch(C, 10.0);
    assert_eq!(b.f.moves(), moves); // ten more seconds of a still person: not one twitch
    let looks = b.looks();
    assert!(looks.windows(2).all(|w| ((w[1].0 - w[0].0).abs() as f64) <= MAX_STEP_DEG + 1.0));
    assert_eq!(looks.last().unwrap().2, 4000); // and the hold is still being renewed
    assert_eq!(b.states(), vec!["seen"]);
}

#[test]
fn test_one_sighting_is_not_a_person_and_an_outlier_does_not_move_the_head() {
    let b = Bench::new();
    b.phase("listening");
    b.see(C, 0.25);
    assert!(b.sent().is_empty() && b.f.track().is_some() && b.track().confirmed == 1);
    b.watch(C, 8.0);
    let settled = b.pose();
    let moves_before = b.f.moves();
    b.see(Some((5.0, 50.0)), 1.5); // one frame says they are 25° away: a blur, a poster
    assert!(b.pose() == settled && b.track().doubt.is_some());
    b.see(C, 0.3);
    assert!(b.pose() == settled && b.track().doubt.is_none() && b.f.moves() == moves_before);
    b.see(Some((5.0, 50.0)), 1.5);
    b.see(Some((5.0, 50.0)), 0.3); // twice in a row: that is where they are now
    assert!(b.pose().0 > settled.0);
}

#[test]
fn test_frames_from_a_swinging_head_are_not_evidence() {
    let b = Bench::new();
    b.phase("listening");
    b.watch(C, 3.0);
    assert!(b.f.moves() >= 1);
    let n = b.n_sent();
    b.set_t(b.f.last_move_at()); // rewind to the moment of the move
    for _ in 0..4 {
        // 1.0 s of frames that claim they are somewhere wild
        b.see(Some((60.0, 70.0)), 0.25);
    }
    assert!(b.n_sent() == n && b.track().doubt.is_none()); // inside SETTLE_SECS: not even an outlier
}

#[test]
fn test_a_walking_person_is_led_not_chased() {
    let b = Bench::new();
    b.phase("speaking");
    let walk = |t: f64| (-30.0 + 6.0 * (t - 100.0), 45.0); // 6°/s to eve's right
    b.watch_fn(|t| Some(walk(t)), 10.0, 0.25);
    let target = walk(b.t()).0;
    assert!(b.track().vyaw > 3.0); // it knows they are moving, and which way
    assert!((b.pose().0 - target).abs() <= 20.0 && b.f.moves() >= 3); // still in frame the whole way
    assert!(!b.states().contains(&"lost".to_string()));
}

#[test]
fn test_a_second_face_does_not_steal_the_gaze() {
    let b = Bench::new();
    b.phase("listening");
    b.watch(C, 6.0);
    let settled = b.pose();
    for _ in 0..12 {
        // a passer-by, bigger and nearer, for three seconds
        let p = b.pose();
        b.see_with(C, 0.25, &[face_at_sized(settled.0 + 25.0, 45.0, p.0, p.1, 45, 95)]);
    }
    assert!((b.pose().0 - settled.0).abs() <= DEADBAND_YAW);
    let fresh = Bench::new();
    fresh.phase("listening");
    for _ in 0..6 {
        // no history: the largest, which is the nearest
        let p = fresh.pose();
        fresh.see_with(Some((-25.0, 45.0)), 0.25, &[face_at_sized(20.0, 45.0, p.0, p.1, 45, 95)]);
    }
    assert!(fresh.pose().0 > 0.0);
}

#[test]
fn test_weak_or_tiny_faces_are_not_people() {
    let b = Bench::new();
    b.phase("listening");
    for _ in 0..8 {
        b.set_t(b.t() + 0.25);
        b.faces(&[Face::new(70, 0, 20, 20), Face::new(-70, 0, 2, 95)], 0.0, 45.0);
    }
    // never a track; the only looks are the empty-map opening scan, which keeps the head's own yaw (a
    // followed face at bx=70 would have turned it)
    assert!(b.f.track().is_none() && b.sent().iter().all(|c| c["yaw"] == 0));
}

#[test]
fn test_an_asked_for_pose_is_the_owners_for_as_long_as_it_is_held() {
    let b = Bench::new();
    b.phase("listening");
    b.watch(C, 6.0);
    b.f.owner_moved(15.0); // "look left" → move_head, hold 15 s
    b.set_pose((-60.0, 45.0));
    let n = b.n_sent();
    b.watch(Some((-50.0, 45.0)), 10.0); // a face right there, and it does not move
    assert_eq!(b.n_sent(), n);
    b.watch(Some((-50.0, 45.0)), 8.0); // the hold has run out
    assert!(b.n_sent() > n && b.f.holding());
}

#[test]
fn test_a_track_that_aged_while_standing_down_is_forgotten_not_searched_on() {
    let b = Bench::new();
    b.phase("listening");
    let start = b.t();
    b.watch_fn(|t| Some((8.0 * (t - start), 45.0)), 4.0, 0.25); // they were drifting right …
    b.f.owner_moved(15.0); // … then "look left", held for 15 s
    assert!(b.f.track().is_none());
    b.set_pose((-70.0, 45.0));
    b.watch_dt(None, 17.0, 0.5); // the hold runs out on an empty frame
    assert!(b.f.searches() == 0 && b.f.search().is_none()); // no search on history
    let thinking = Bench::new();
    thinking.phase("listening");
    thinking.watch(C, 6.0);
    thinking.phase("thinking");
    thinking.watch_dt(None, 12.0, 0.5); // a long think: the follower is standing down
    thinking.phase("speaking");
    thinking.watch_dt(None, 3.0, 0.5);
    assert!(thinking.f.searches() == 0 && thinking.f.track().is_none());
}

#[test]
fn test_leaving_a_follow_phase_hands_the_head_back_at_once() {
    let b = Bench::new();
    b.phase("listening");
    b.watch(C, 6.0);
    b.phase("thinking"); // the board glances aside here: let it
    assert!(b.looks().last().unwrap().2 == 0 && !b.f.holding());
    let why = Arc::new(Mutex::new(String::new()));
    let w = why.clone();
    let c = Bench::with(move |o| o.blocked = Arc::new(move || w.lock().clone()));
    c.phase("listening");
    c.watch(C, 6.0);
    *why.lock() = "explore".into();
    c.see(C, 1.0);
    assert!(c.looks().last().unwrap().2 == 0 && !c.f.holding()); // someone else owns the head: released, then silent
    let n = c.n_sent();
    c.watch(C, 3.0);
    assert_eq!(c.n_sent(), n);
}

#[test]
fn test_someone_who_leaves_fast_is_followed_the_way_they_went() {
    let b = Bench::new();
    b.phase("listening");
    b.watch(Some((0.0, 45.0)), 4.0);
    let start = b.t();
    // 40°/s to eve's right: out of frame within a second
    b.watch_fn(|t| Some((40.0 * (t - start), 45.0)), 1.0, 0.25);
    let gone_to = Some((70.0, 45.0)); // where they stopped, far outside the frame
    b.watch(None, 0.5);
    b.watch(gone_to, 9.0);
    assert!(b.states().contains(&"found:where they were heading".to_string()), "{:?}", b.states());
    assert!((b.pose().0 - 70.0).abs() <= 20.0 && b.f.found_by() == vec!["where they were heading"]);
    let lost_at = b.s.lock().events.iter().find(|(_t, m)| m["state"] == "lost").map(|(_t, m)| m["time"].as_f64().unwrap());
    assert!(lost_at.is_some_and(|t| t != 0.0) && b.f.searches() == 1);
}

#[test]
fn test_someone_who_vanishes_from_stillness_is_looked_for_where_they_usually_are() {
    let mut usual = PresenceMap::new(None);
    for _ in 0..40 {
        usual.note(45.0, 60.0); // they are often standing over to the right
    }
    for _ in 0..25 {
        usual.note(-23.0, 38.0); // … and usually in the chair
    }
    let b = Bench::with(|o| o.presence = Some(usual));
    b.phase("listening");
    b.watch(C, 6.0);
    let order = dedupe(whys(&b.f.plan_search(&b.track())));
    assert_eq!(order, vec!["where they usually are", "either side", "up and down"]); // what it has learned comes first
    b.watch(None, 3.0); // still, then gone: patience first
    assert!(b.f.search().is_some() && b.f.searches() == 1);
    let s = b.f.search().unwrap();
    let mut plan = whys(&s.places);
    plan.push(s.looking.clone());
    // they were still: no guess at a heading
    assert!(plan.contains(&"where they usually are".to_string()) && !plan.contains(&"where they were heading".to_string()));
    let cold = Bench::new(); // nothing learned yet: either side comes first
    cold.phase("listening");
    cold.watch(C, 6.0);
    assert_eq!(whys(&cold.f.plan_search(&cold.track()))[..2], ["either side", "either side"]);
    b.watch(Some((45.0, 60.0)), 12.0);
    assert!(b.states().contains(&"found:where they usually are".to_string()), "{:?}", b.states());
    assert!((b.pose().0 - 45.0).abs() <= 12.0);
}

#[test]
fn test_a_conversation_that_opens_on_an_empty_frame_looks_where_they_usually_are() {
    let chair_map = || {
        let mut usual = PresenceMap::new(None);
        for _ in 0..30 {
            usual.note(CHAIR.0, CHAIR.1);
        }
        usual
    };
    let b = Bench::with(|o| o.presence = Some(chair_map()));
    b.phase("wake");
    let far = Some((-45.0, 38.0)); // out of view from the board's fixed pose (0, 45)
    b.watch(far, 1.0);
    assert!(b.sent().is_empty()); // patience first
    b.watch(far, 6.0);
    assert!(b.looks()[0].0 == -20 && b.looks()[0].1 == 40 && b.f.acquired()); // one look at the chair …
    assert!(b.f.holding() && (b.pose().0 - -45.0).abs() <= 12.0); // … and from there it sees them and follows
    assert_eq!(b.f.searches(), 0); // that look is not a search
    let empty = Bench::new(); // nothing learned yet: scan down from the head's pose
    empty.phase("wake");
    empty.watch(None, 1.0);
    assert!(empty.sent().is_empty()); // patience first
    empty.watch(None, 6.0);
    let first: Vec<(i64, i64)> = empty.looks().iter().take(3).map(|l| (l.0, l.1)).collect();
    assert_eq!(first, vec![(0, 27), (0, 9), (0, 63)]);
    let seen = Bench::with(|o| o.presence = Some(chair_map())); // someone already in view: the look is never spent
    seen.phase("wake");
    seen.watch(Some((3.0, 45.0)), 6.0);
    assert!(seen.looks().iter().all(|l| l.0.abs() <= 8));
}

/// 2026-09-27: the level pose looked over the seated owner (frames held the top of his hair), and a face
/// at a frame's edge is not detected. The opening looks where they usually are, then lower.
#[test]
fn test_a_seated_owner_below_the_usual_place_is_found_by_looking_a_little_lower() {
    let mut usual = PresenceMap::new(None);
    for _ in 0..30 {
        usual.note(-10.0, 30.0);
    }
    let b = Bench::with(|o| o.presence = Some(usual));
    let seated = (-10.0, 12.0);
    b.phase("listening");
    for _ in 0..40 {
        // only a face well inside the frame is detected
        b.set_t(b.t() + 0.25);
        let p = b.pose();
        let near = (seated.1 - p.1).abs() <= 12.0 && (seated.0 - p.0).abs() <= 20.0;
        let faces = if near { vec![face_at(seated.0, seated.1, p.0, p.1)] } else { vec![] };
        b.faces(&faces, p.0, p.1);
    }
    let looks = b.looks();
    assert!((looks[0].0, looks[0].1) == (-10, 30) && (looks[1].0, looks[1].1) == (-10, 12));
    assert!(b.f.holding() && (b.pose().1 - seated.1).abs() <= DEADBAND_PITCH);
}

/// A desk call hands the head back for each "thinking"; the board's own pose then looks over the owner.
/// Coming back to speak, eve looks for them again, at most MAX_OPENINGS times a conversation.
#[test]
fn test_every_return_from_thinking_with_nobody_in_view_gets_a_fresh_opening() {
    let mut usual = PresenceMap::new(None);
    for _ in 0..30 {
        usual.note(CHAIR.0, CHAIR.1);
    }
    let b = Bench::with(|o| o.presence = Some(usual));
    for turn in 0..MAX_OPENINGS + 2 {
        b.phase(if turn == 0 { "listening" } else { "speaking" });
        b.watch(None, 6.0);
        b.phase("thinking");
        b.set_pose((0.0, 45.0)); // the board takes the head back to level
        b.watch(None, 1.0);
    }
    let starts = b
        .sent()
        .iter()
        .filter(|c| c["cmd"] == "look" && c["hold"].as_i64().unwrap() > 0 && c["yaw"] == -20 && c["pitch"] == 40)
        .count();
    assert_eq!(starts, MAX_OPENINGS);
    b.phase("speaking");
    b.watch(C, 6.0); // someone in view is still followed after that
    assert!(b.f.holding() && (b.pose().0 - CHAIR.0).abs() <= DEADBAND_YAW);
}

/// 2026-09-27: the map held yaw 120 pitch 0 (10° bins, learned off refused looks) and the opening look
/// turned the head right round to the wall behind it.
#[test]
fn test_a_learned_place_past_the_limits_is_brought_back_inside_them() {
    let mut usual = PresenceMap::new(None);
    for _ in 0..30 {
        usual.note(118.0, 2.0);
    }
    let b = Bench::with(|o| o.presence = Some(usual));
    b.phase("listening");
    b.watch(None, 4.0);
    let looks = b.looks();
    assert!(!looks.is_empty() && looks.iter().all(|&(y, p, _)| y.abs() <= MAX_YAW && (5..=85).contains(&p)));
}

/// The board keeps the head when it wants the owner's attention (hostlook.h). The follower must then
/// build on the pose the board echoes, not the one it asked for — or a face that never moves in the
/// frame walks the ask to yaw 100 (live, 2026-09-27 09:49).
#[test]
fn test_a_refused_look_is_noticed_and_the_head_is_not_walked_to_its_limits() {
    let b = Bench::new();
    b.set_ok(true);
    b.phase("listening");
    let real = (0.0, 45.0);
    for _ in 0..40 {
        // the board answers every look but never moves
        b.set_t(b.t() + 0.25);
        b.faces(&[face_at(20.0, 45.0, real.0, real.1)], real.0, real.1);
    }
    let asked: Vec<i64> = b.looks().iter().map(|l| l.0).collect();
    assert!((*asked.iter().max().unwrap() as f64) <= 20.0 + DEADBAND_YAW);
    let b = Bench::new();
    b.phase("listening");
    b.see(Some((-4.0, 38.0)), 0.25);
    b.see(Some((0.0, 45.0)), 0.5); // two boxes 8° apart: "9°/s", from a person sitting still
    assert!(b.track().confirmed == 2 && b.track().speed() > 2.0); // a spurious velocity, from box jitter alone
    assert!(!whys(&b.f.plan_search(&b.track())).contains(&"where they were heading".to_string()));
}

#[test]
fn test_it_gives_the_head_back_and_does_not_keep_scanning_an_empty_room() {
    let b = Bench::new();
    b.phase("listening");
    b.watch(C, 6.0);
    b.watch_dt(None, 120.0, 0.5); // two minutes of nobody
    // ONE search per loss, then it waits
    assert!(b.f.searches() == 1 && !b.f.holding() && b.looks().last().unwrap().2 == 0);
    let held: Vec<(i64, i64, i64)> = b.looks().into_iter().filter(|l| l.2 == 4000).collect();
    let search_looks = &held[held.len().saturating_sub(MAX_PLACES)..];
    let distinct: std::collections::HashSet<(i64, i64)> = search_looks.iter().map(|l| (l.0, l.1)).collect();
    assert!(distinct.len() <= MAX_PLACES);
    assert_eq!(b.states().iter().filter(|s| *s == "lost").count(), 1);
    b.watch(C, 8.0); // they come back by themselves: followed again …
    assert!(b.f.holding() && b.states().last().map(String::as_str) == Some("seen"));
    b.watch_dt(None, 20.0, 0.5); // … and a second loss earns a second search
    assert_eq!(b.f.searches(), 2);
    b.phase("idle");
    assert!(b.f.searches() == 0 && b.f.track().is_none()); // the next conversation starts fresh
}

#[test]
fn test_the_presence_map_learns_decays_and_survives_a_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = Arc::new(Mutex::new(1_000_000.0));
    let path = tmp.path().join("presence.json");
    let mut m = PresenceMap::with_wall(Some(path.clone()), wall_of(&clock));
    for _ in 0..10 {
        m.note(-23.4, 38.2);
    }
    for _ in 0..4 {
        m.note(44.0, 61.0);
    }
    m.note(90.0, 45.0); // once: not a place, a passing glance
    assert_eq!(m.top(2), vec![(-20, 40), (40, 60)]);
    assert_eq!(m.top_excluding(2, &[(-23.0, 38.0)]), vec![(40, 60)]);
    assert!(m.save() && read_json(&path)["cells"]["-20,40"] == 10.0);
    let mut again = PresenceMap::with_wall(Some(path.clone()), wall_of(&clock)).load();
    assert!(again.top(1) == vec![(-20, 40)] && !again.save()); // nothing new to write
    *clock.lock() += 28.0 * 86400.0; // four weeks on: two half-lives
    let mut later = PresenceMap::with_wall(Some(path.clone()), wall_of(&clock)).load();
    let w = later.cells[&(-20, 40)];
    assert!((2.4..=2.6).contains(&w) && later.top(2).is_empty()); // faded below "usually"
    assert!(PresenceMap::new(Some(tmp.path().join("missing.json"))).load().cells.is_empty());
    std::fs::write(tmp.path().join("bad.json"), "{not json").unwrap();
    assert!(PresenceMap::new(Some(tmp.path().join("bad.json"))).load().cells.is_empty());
    let loaded = PresenceMap::with_wall(Some(path.clone()), wall_of(&clock)).load();
    let b = Bench::with(|o| o.presence = Some(loaded));
    b.phase("listening");
    b.watch(C, 6.0);
    b.phase("idle"); // a conversation's worth of sightings is kept
    assert!(read_json(&path)["cells"]["-20,40"].as_f64().unwrap() > 2.6);
}

#[test]
fn test_it_never_looks_behind_itself_and_survives_a_dead_link_the_switch_and_the_bus() {
    let b = Bench::new();
    b.phase("listening");
    b.set_pose((95.0, 45.0));
    b.watch(Some((125.0, 45.0)), 8.0);
    assert!(b.looks().iter().map(|l| l.0).max().unwrap() <= MAX_YAW);
    let dead = Bench::new();
    dead.set_ok(false);
    dead.phase("listening");
    dead.watch(C, 4.0);
    assert!(!dead.f.holding() && dead.f.moves() == 0); // the move did not reach the robot: nothing is claimed
    let off = Bench::with(|o| o.enabled = false);
    off.phase("listening");
    off.watch(C, 4.0);
    assert!(off.sent().is_empty());
    assert!(configured(Some(&HashMap::new())));
    let env = |k: &str, v: &str| HashMap::from([(k.to_string(), v.to_string())]);
    assert!(!configured(Some(&env("EVE_FOLLOW_SPEAKER", "0"))));
    assert_eq!(presence_path(Some(&env("EVE_PRESENCE_FILE", "/tmp/p.json"))), PathBuf::from("/tmp/p.json"));
    let none = Bench::new();
    none.phase("listening");
    block_on(none.f.on_faces(&[Face::new(60, 0, 20, 90)], None, None)); // a frame without its pose cannot be placed
    assert!(none.sent().is_empty());

    let deaf = Bench::with(|o| o.publish = Some(Arc::new(|_topic: &str, _msg| Err("bus down".to_string()))));
    deaf.phase("listening");
    deaf.watch(C, 6.0);
    assert!(deaf.f.holding()); // the bus is an audience, never a dependency
    let panicky = Bench::with(|o| o.publish = Some(Arc::new(|_topic: &str, _msg| panic!("bus down"))));
    panicky.phase("listening");
    panicky.watch(C, 6.0);
    assert!(panicky.f.holding());
}

#[test]
fn test_an_asked_for_pose_reaches_the_follower_through_the_head_and_presence_is_a_bus_topic() {
    use eve_head::Head;

    let sent = Arc::new(Mutex::new(Vec::<Value>::new()));
    let s2 = sent.clone();
    let send: Sender = Arc::new(move |cmd| {
        let s = s2.clone();
        Box::pin(async move {
            s.lock().push(cmd);
            true
        })
    });
    let fail: Sender = Arc::new(|_cmd| Box::pin(async { false }));
    let holds = Arc::new(Mutex::new(Vec::<f64>::new()));
    let h2 = holds.clone();
    let head = Head::new(send.clone());
    head.set_on_move(Some(Arc::new(move |h| h2.lock().push(h))));
    assert_eq!(block_on(head.move_to(Some(-60.0), Some(45.0), false, 15.0))["ok"], true);
    assert_eq!(*holds.lock(), vec![15.0]);
    let failing = Head::new(fail);
    let h3 = holds.clone();
    failing.set_on_move(Some(Arc::new(move |h| h3.lock().push(h))));
    assert_eq!(block_on(failing.move_to(Some(10.0), Some(45.0), false, 15.0))["ok"], false);
    assert_eq!(*holds.lock(), vec![15.0]); // a move that did not land is not a hold
    assert!(eve_memory_bus::TOPICS.iter().any(|(t, _)| *t == PRESENCE_TOPIC));

    // and the follower, wired as the daemon wires it, stands down for that hold
    let t = Arc::new(Mutex::new(100.0));
    let t2 = t.clone();
    let follower = Arc::new(SpeakerFollower::with(
        send,
        FollowerOptions { clock: Arc::new(move || *t2.lock()), ..FollowerOptions::default() },
    ));
    let f2 = follower.clone();
    head.set_on_move(Some(Arc::new(move |h| f2.owner_moved(h))));
    block_on(follower.on_phase("listening"));
    assert!(follower.active());
    block_on(head.move_to(Some(-60.0), Some(45.0), false, 15.0));
    assert!(!follower.active() && follower.owner_hold_until() == 115.0);
    *t.lock() = 115.0;
    assert!(follower.active());
}

// ---- test_desk_call.py: the head follows whoever is talking on a desk call ----

#[test]
fn test_during_a_desk_call_eve_turns_to_the_face_it_sees() {
    let t = Arc::new(Mutex::new(100.0));
    let looks = Arc::new(Mutex::new(Vec::<Value>::new()));
    let l2 = looks.clone();
    let board: Sender = Arc::new(move |obj: Value| {
        let l = l2.clone();
        Box::pin(async move {
            if obj["cmd"] == "look" {
                l.lock().push(obj);
            }
            true
        })
    });
    let t2 = t.clone();
    let follower =
        SpeakerFollower::with(board, FollowerOptions { clock: Arc::new(move || *t2.lock()), ..FollowerOptions::default() });
    block_on(follower.on_phase("listening")); // the desk call's first phase
    assert!(follower.active());
    for _ in 0..12 {
        // the owner sits well to eve's left of where the head points
        *t.lock() += 0.25;
        block_on(follower.on_faces(&[Face::new(-60, 10, 22, 90)], Some(0.0), Some(45.0)));
    }
    let looks = looks.lock().clone();
    assert!(!looks.is_empty() && looks[0]["yaw"].as_i64().unwrap() < -10);
    block_on(follower.on_phase("idle")); // the call ends
    assert!(!follower.active() && follower.phase() == "idle");
}

// ---- Rust-only checks of the parity details ----

#[test]
fn test_saved_file_is_byte_for_byte_what_python_writes() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("sub").join("presence.json");
    let mut m = PresenceMap::with_wall(Some(path.clone()), Arc::new(|| 1_000_000.0));
    for _ in 0..10 {
        m.note(-23.4, 38.2);
    }
    for _ in 0..4 {
        m.note(44.0, 61.0);
    }
    m.note(90.0, 45.0); // 4.5 rounds half to even: the 40 cell, as Python's round()
    m.note_weight(1.0, 1.0, 1.0 / 3.0);
    assert!(m.save());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        r#"{"stamp": 1000000.0, "bin_deg": 10, "cells": {"-20,40": 10.0, "40,60": 4.0, "90,40": 1.0, "0,0": 0.33}}"#
    );
    assert!(!tmp.path().join("sub").join("presence.tmp").exists());
}

#[test]
fn test_load_reads_what_python_reads_and_refuses_what_it_refuses() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("p.json");
    let load = |body: &str| {
        std::fs::write(&p, body).unwrap();
        PresenceMap::with_wall(Some(p.clone()), Arc::new(|| 5000.0)).load()
    };
    let m = load(r#"{"stamp": 5000, "cells": {" -20 , 40": "3.5", "10,20,30": true, "1_0,0": 2}}"#);
    assert_eq!(m.cells.iter().map(|(c, w)| (*c, *w)).collect::<Vec<_>>(), vec![((-20, 40), 3.5), ((10, 20), 1.0), ((10, 0), 2.0)]);
    assert_eq!(m.stamp, 5000.0);
    let m = load(r#"{"cells": {"0,0": 4}, "stamp": 0}"#); // a falsy stamp is the wall clock
    assert!(m.cells.len() == 1 && m.stamp == 5000.0);
    assert!(load(r#"{"cells": {"0": 4}}"#).cells.is_empty()); // IndexError
    assert!(load(r#"{"cells": {"0,x": 4}}"#).cells.is_empty()); // ValueError
    assert!(load(r#"{"cells": {"0,0": null}}"#).cells.is_empty()); // TypeError
    assert!(load(r#"{"cells": {"0,0": 4}, "stamp": "soon"}"#).cells.is_empty());
    assert!(load(r#"{"cells": []}"#).cells.is_empty());
    assert!(load(r#"[1, 2]"#).cells.is_empty());
}

#[test]
fn test_python_number_formats() {
    assert_eq!(py_float_repr(10.0), "10.0");
    assert_eq!(py_float_repr(0.33), "0.33");
    assert_eq!(py_float_repr(1760000000.123456), "1760000000.123456");
    assert_eq!(py_float_repr(1e16), "1e+16");
    assert_eq!(py_float_repr(1.5e-5), "1.5e-05");
    assert_eq!(py_float_repr(0.0001), "0.0001");
    assert_eq!(py_round_to(2.675, 2), 2.67); // the binary value is below 2.675, as Python rounds it
    assert_eq!(py_round_to(-23.25, 1), -23.2);
    assert_eq!(py_int(" +7 "), Some(7));
    assert_eq!(py_int("1__0"), None);
    assert_eq!(PresenceMap::cell(-25.0, 35.0), (-20, 40));
    assert_eq!(clip(f64::NAN, -100.0, 100.0), 100.0);
}

#[test]
fn test_configured_and_presence_path_read_the_environment_like_python() {
    let env = |v: &str| HashMap::from([("EVE_FOLLOW_SPEAKER".to_string(), v.to_string())]);
    for off in ["0", " OFF ", "False", "no"] {
        assert!(!configured(Some(&env(off))), "{off}");
    }
    for on in ["", "1", "yes", "anything"] {
        assert!(configured(Some(&env(on))), "{on}");
    }
    let home = Some(OsString::from("/Users/someone/"));
    assert_eq!(expanduser(DEFAULT_PRESENCE_FILE, home.clone()), PathBuf::from("/Users/someone/.config/eve-bridge/presence.json"));
    assert_eq!(expanduser("~", home.clone()), PathBuf::from("/Users/someone"));
    assert_eq!(expanduser("/abs/p.json", home), PathBuf::from("/abs/p.json"));
    let empty = HashMap::from([("EVE_PRESENCE_FILE".to_string(), String::new())]);
    assert!(presence_path(Some(&empty)).ends_with(".config/eve-bridge/presence.json"));
}

#[test]
fn test_owner_moved_with_a_nan_or_negative_hold_ends_at_once() {
    let b = Bench::new();
    b.phase("listening");
    b.f.owner_moved(f64::NAN);
    assert!(b.f.active());
    b.f.owner_moved(-5.0);
    assert!(b.f.active());
}

#[test]
fn test_the_followers_futures_are_send() {
    fn is_send<T: Send>(_: &T) {}
    let b = Bench::new();
    let faces = [Face::new(0, 0, 20, 90)];
    is_send(&b.f.on_faces(&faces, Some(0.0), Some(45.0)));
    is_send(&b.f.on_phase("listening"));
    is_send(&b.f.release("why"));
}

// ---- vision's FaceTracker feeds the follower ----

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_the_tracker_hands_the_follower_every_face_with_the_frames_pose_and_never_dies_of_it() {
    use base64::Engine as _;
    use eve_vision::{FaceTracker, OnFaces, Rect};

    let got = Arc::new(Mutex::new(Vec::<(Vec<(i64, i64)>, Option<f64>, Option<f64>)>::new()));
    let sent = Arc::new(Mutex::new(Vec::<Value>::new()));
    let s2 = sent.clone();
    let send: Sender = Arc::new(move |cmd: Value| {
        let s = s2.clone();
        Box::pin(async move {
            s.lock().push(cmd);
            true
        })
    });
    let g2 = got.clone();
    let on_faces: OnFaces = Arc::new(move |faces, yaw, pitch| {
        let g = g2.clone();
        Box::pin(async move {
            // the daemon's mapping, vision's FaceResult to the follower's Face
            let faces: Vec<Face> = faces.iter().map(|f| Face::new(f.bx, f.by, f.size, f.conf)).collect();
            g.lock().push((faces.iter().map(|f| (f.bx, f.size)).collect(), yaw, pitch));
            anyhow::bail!("a follower bug") // must not stop the tracker
        })
    });
    let rects = vec![Rect::with_conf(10.0, 30.0, 40.0, 40.0, 0.9), Rect::with_conf(110.0, 40.0, 20.0, 20.0, 0.8)];
    let tracker = FaceTracker::new(Some(Arc::new(move |_f: &eve_vision::Frame| Ok(rects.clone()))), send);
    tracker.set_on_faces(Some(on_faces));
    let frame = serde_json::json!({"seq": 7, "w": 160, "h": 120, "fmt": "gray", "yaw": 12, "pitch": 40,
        "b64": base64::engine::general_purpose::STANDARD.encode(vec![0u8; 160 * 120])});
    tracker.on_frame(&frame).await;
    for _ in 0..200 {
        // the tracker works the frame on its own task: wait for it to finish
        if !got.lock().is_empty() && !sent.lock().is_empty() && !tracker.governor().busy {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    tracker.stop();
    assert_eq!(*got.lock(), vec![(vec![(-62, 25), (50, 12)], Some(12.0), Some(40.0))]); // both faces, left then right, with the pose
    let sent = sent.lock();
    // the board still gets the largest, as before
    assert!(sent.iter().map(|c| c["cmd"].as_str().unwrap()).eq(["face"]) && sent[0]["seq"] == 7);
}
