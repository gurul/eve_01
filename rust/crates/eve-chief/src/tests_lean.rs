//! test_chief_lean.py: the chief's Lean model (verification/Eve/Chief.lean), replayed against this chief: gate G8.3
//! of the chief design (2026-09-29).
//!
//! The model's decision rules are transliterated here one to one (`MPhase` … `step_with`, each citing its Lean
//! definition), with the constants read out of the .lean file itself, so a change to either side shows up as a
//! disagreement. The transliteration is first held to the model's own kernel-checked traces (`desk_job_closes`,
//! `timeout_is_no`, … `no_act_at_night`): what Lean proved of the model, the transliteration must reproduce.
//!
//! Then seeded random traces drive the real `Chief` with fakes the test controls: when each executor answers and
//! how (a web read with four links, a failed one; an options sheet, a reply that is not one; a Mac act that saw
//! "Order confirmed" on screen, one whose only claim is its own sentence, one that failed), the owner's answers
//! and code words (Yes, No, silence, a hold word, "go c1", change, drop, done, reopen, It happened, It didn't,
//! Raise), the clock (quiet-hour edges), and restarts (a new Chief on the same folder). Every ledger line the code
//! writes becomes the model's event, and after every move the two must agree:
//!
//! - **dispatch**: the code starts a step exactly when the model's `dispatch` of the first unfinished phase would
//!   (canGo, fits, quiet hours), and it is the same step;
//! - **approval**: every yes the code records (by id or to a Go) the model's `fGo` / `fAnswer` also grants, and the
//!   token is there on both sides or on neither; every Go the code asks the model's `fAsk` allows;
//! - **proof**: every `checked` verdict equals `verdictOf` of the evidence the fakes actually produced (the test
//!   knows it: it wrote them), and the card's status (active, unverified, done, dropped), revision, running step,
//!   each phase's state, attempts and retries, and the lessons kept per (executor, phase kind) are equal;
//! - the model's `Spec` (all seven properties) holds after every event.
//!
//! Positive control (design 9 P8): a variant of the code whose act adapter takes the executor's own sentence for a
//! screen state (chief_receipt::from_agent replaced, through the chief's test seam) closes a card done on the
//! executor's word; the replay finds the disagreement at that check, and the same variant agrees instead with the
//! model's `naiveVerdict` oracle, whose end state breaks DoneNeedsEvidence, as `naive_violates` proves in Lean.
//!
//! Where the model and the code differ in shape and not in the properties, the replay maps one onto the other
//! (the six notes of test_chief_lean.py, unchanged):
//!
//! 1. A failed one-way act: the model puts it back to waiting for a Go on an active card (Chief.lean `fReflect`);
//!    the code leaves it failed, closes the card unverified at the next tick, and Reopen then asks a new Go. Both
//!    never run it again without a new yes. The replay lets a code `failed` one-way phase stand for a model
//!    `queued` one.
//! 2. "go c1" on an act in doubt: the code approves it by id; the model's `fGo` only approves a queued or asked
//!    phase, so the replay feeds it as It didn't, then go. The code keeps offering It happened / It didn't on that
//!    act until it runs again; the replay does not press them then.
//! 3. A retry: the code also re-checks the budget before it re-queues (`reflect`); the model leaves that to
//!    `dispatch`. The replay passes the reflection's word AND the model's own `fits` as `reflect`'s `retry`.
//! 4. Spend: the model's `result` cost is what the code charged (the card's spend before and after, in whole
//!    micro-dollars and seconds, rounded up: the budget thresholds are whole numbers).
//! 5. A change on an unverified card: `Chief::change` makes a new revision and keeps it unverified; the model's
//!    `revise` changes only an active card. The replay compares revisions up to the count of such changes.
//! 6. The desk: the model's `atBreak` is realized by a breakpoint at the moment of the offer on a Desk reloaded
//!    from the same folder; `busy` and `restart` are the absence of one. With pushes off the model drops an
//!    unprompted message where the code batches it (`push_off`); in shadow the code logs a would-push only where
//!    pushes on would have sent one, so it is stricter than the model's `silent`. Neither ever sends it.
//!
//! Not generated, each for the reason test_chief_lean.py gives: a Mac act left running past its 10-minute ceiling,
//! "reopen" on a dropped card, the model's `raise`, and Reopen while a step of a closed card is still running
//! (`test_a_card_closed_during_a_web_step_then_reopened_moves_on`).
//!
//! The traces are seeded from a SplitMix64 generator, not CPython's Mersenne Twister, so seed N here is not
//! seed N there; the coverage test holds the generator to the same list of rules reached.

use super::*;
use crate::tests::{desk_job, env, LINKS, SAID};
use chrono::{NaiveDate, NaiveDateTime, TimeDelta, TimeZone, Timelike};
use eve_chief_desk::{Mandate, PUSHES_PER_DAY, QUIET_END, QUIET_START};
use eve_chief_reflect::AskError;
use std::sync::OnceLock;
use tokio::sync::oneshot;

const LEAN: &str = include_str!("../../../../verification/Eve/Chief.lean");

// ---- the constants, read from the .lean file ----------------------------------------------------------------

/// A Lean Nat literal of the form `22 * 60 + 30`: digits, `*` and `+` only.
fn nat(expr: &str) -> i64 {
    assert!(Regex::new(r"^[0-9 *+]+$").unwrap().is_match(expr), "{expr}");
    expr.split('+').map(|term| term.split('*').map(|f| f.trim().parse::<i64>().unwrap()).product::<i64>()).sum()
}

struct K {
    quiet_from: i64,
    quiet_to: i64,
    push_budget: i64,
    max_retries: i64,
    omega: i64,
}

fn lean_constants(text: &str) -> K {
    let get = |name: &str| {
        let re = Regex::new(&format!(r"(?m)^def {name} : Nat := ([0-9 *+]+?)\s*(?:--.*)?$")).unwrap();
        nat(&re.captures(text).unwrap_or_else(|| panic!("{name} is not in Chief.lean"))[1])
    };
    K {
        quiet_from: get("QUIET_FROM"),
        quiet_to: get("QUIET_TO"),
        push_budget: get("PUSH_BUDGET"),
        max_retries: get("MAX_RETRIES"),
        omega: get("OMEGA"),
    }
}

static KK: LazyLock<K> = LazyLock::new(|| lean_constants(LEAN));

fn hhmm(text: &str) -> i64 {
    let (h, m) = text.split_once(':').unwrap();
    h.parse::<i64>().unwrap() * 60 + m.parse::<i64>().unwrap()
}

fn local_ts(t: NaiveDateTime) -> f64 {
    chrono::Local.from_local_datetime(&t).earliest().unwrap().timestamp() as f64
}

fn day0() -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 9, 29).unwrap().and_hms_opt(0, 0, 0).unwrap()
}

#[test]
fn test_the_models_constants_are_the_codes() {
    // Chief.lean:109-112 against chief_desk and chief_reflect
    let k = &*KK;
    assert_eq!(
        (k.quiet_from, k.quiet_to, k.push_budget, k.max_retries, k.omega),
        (
            hhmm(QUIET_START),
            hhmm(QUIET_END),
            PUSHES_PER_DAY,
            eve_chief_reflect::MAX_RETRIES as i64,
            eve_chief_reflect::OMEGA as i64
        )
    );
    assert_eq!((k.quiet_from, k.quiet_to, k.push_budget, k.max_retries, k.omega), (1350, 480, 2, 2, 3));
}

#[test]
fn test_the_models_quiet_hours_are_the_codes_for_every_minute() {
    // Chief.lean:115 `quiet` against chief_desk::in_quiet, all 1440 minutes of a day
    for m in 0..1440 {
        let ts = local_ts(day0() + TimeDelta::minutes(m));
        assert_eq!(quiet(m), in_quiet(&Mandate::default(), ts).unwrap(), "{m}");
    }
}

// ---- the model, transliterated (Chief.lean:107-515) ----------------------------------------------------------

/// (micro-dollars, seconds): Chief.lean:190 `Cost`
type Cost = (i64, i64);
const ZERO: Cost = (0, 0);

/// Chief.lean:361 `verdictOf`; `naive`: Chief.lean:978 `naiveVerdict`.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Vf {
    Verdict,
    Naive,
}

fn verdict(vf: Vf, ev: &str) -> &'static str {
    match ev {
        "observed" => "confirmed",
        "contradicted" => "failed",
        "executorText" if vf == Vf::Naive => "confirmed",
        "executorText" | "missing" => "unverifiable",
        _ => panic!("{ev}"),
    }
}

fn quiet(m: i64) -> bool {
    // Chief.lean:115
    KK.quiet_from <= m || m < KK.quiet_to
}

/// Chief.lean:224
#[derive(Clone, Debug, PartialEq)]
struct MPhase {
    kind: String,
    /// "two" | "one"
    door: &'static str,
    /// queued | asked | running | ok | failed | doubt
    st: &'static str,
    token: Option<i64>,
    ask_rev: Option<i64>,
    attempts: i64,
    retries: i64,
    yeses: i64,
    ow_acts: i64,
    last_yes: Option<i64>,
    stale: bool,
    doubt: bool,
    replay: bool,
}

impl MPhase {
    fn new(kind: &str, door: &'static str) -> Self {
        MPhase {
            kind: kind.into(),
            door,
            st: "queued",
            token: None,
            ask_rev: None,
            attempts: 0,
            retries: 0,
            yeses: 0,
            ow_acts: 0,
            last_yes: None,
            stale: false,
            doubt: false,
            replay: false,
        }
    }
}

/// Chief.lean:240
#[derive(Clone, Debug)]
struct MCard {
    cap: Cost,
    rev: i64,
    /// active | unverified | done | dropped
    status: &'static str,
    phases: Vec<MPhase>,
    checks: Vec<(&'static str, bool)>,
    by_owner: bool,
    running: Option<usize>,
    held: Cost,
    spent: Cost,
    over: Cost,
}

/// Chief.lean:253
#[derive(Clone, Debug)]
struct MDesk {
    mode: &'static str,
    clock: i64,
    at_break: bool,
    waited: bool,
    pushes: i64,
    route: &'static str,
}

/// Chief.lean:262
#[derive(Clone, Debug)]
struct MS {
    card: MCard,
    desk: MDesk,
    lessons: Vec<i64>,
    quiet_act: bool,
    bad_push: bool,
    sent_today: i64,
}

/// The model's events (Chief.lean `Ev`).
#[derive(Clone, Debug)]
enum E {
    Clock(i64),
    NewDay,
    Breakpoint,
    Waited,
    Busy,
    Restart(Cost),
    Offer(&'static str),
    Ask(i64),
    Answer(i64, String),
    GoId(i64),
    Revise,
    Dispatch(i64),
    Result(i64, bool, Cost),
    Reflect(i64, i64, bool),
    Requeue(i64),
    Happened(i64),
    Didnt(i64),
    Oracle(i64, &'static str),
    Close,
    OwnerDone,
    Reopen,
    Drop,
    RaiseBudget(Cost),
}

fn start(phases: &[(&str, &'static str)], checks: usize, cap: Cost, mode: &'static str) -> MS {
    // Chief.lean:279
    MS {
        card: MCard {
            cap,
            rev: 1,
            status: "active",
            phases: phases.iter().map(|(k, d)| MPhase::new(k, d)).collect(),
            checks: vec![("pending", false); checks],
            by_owner: false,
            running: None,
            held: ZERO,
            spent: ZERO,
            over: ZERO,
        },
        desk: MDesk { mode, clock: 0, at_break: false, waited: false, pushes: 0, route: "batch" },
        lessons: Vec::new(),
        quiet_act: false,
        bad_push: false,
        sent_today: 0,
    }
}

fn upd<T: Clone>(f: impl Fn(&T) -> T, n: i64, xs: &[T]) -> Vec<T> {
    // Chief.lean:285
    xs.iter().enumerate().map(|(i, x)| if i as i64 == n { f(x) } else { x.clone() }).collect()
}

fn at_<T>(xs: &[T], n: i64) -> Option<&T> {
    if n >= 0 {
        xs.get(n as usize)
    } else {
        None
    }
}

fn approve_m(r: i64, p: &MPhase) -> MPhase {
    // Chief.lean:293
    MPhase { st: "queued", ask_rev: None, token: Some(r), yeses: p.yeses + 1, last_yes: Some(r), doubt: false, ..p.clone() }
}

fn f_ask(r: i64, p: &MPhase) -> MPhase {
    // Chief.lean:297
    if p.door == "one" && p.st == "queued" && p.token.is_none() {
        MPhase { st: "asked", ask_rev: Some(r), ..p.clone() }
    } else {
        p.clone()
    }
}

fn f_answer(r: i64, a: &str, p: &MPhase) -> MPhase {
    // Chief.lean:302
    if p.st != "asked" {
        return p.clone();
    }
    if a == "yes" && p.ask_rev == Some(r) && p.door == "one" {
        return approve_m(r, p);
    }
    MPhase { st: "queued", ask_rev: None, ..p.clone() }
}

fn f_go(r: i64, p: &MPhase) -> MPhase {
    // Chief.lean:309
    if p.door == "one" && (p.st == "queued" || p.st == "asked") {
        approve_m(r, p)
    } else {
        p.clone()
    }
}

fn f_clear(p: &MPhase) -> MPhase {
    // Chief.lean:313
    MPhase { token: None, ask_rev: None, st: if p.st == "asked" { "queued" } else { p.st }, ..p.clone() }
}

fn can_go(r: i64, p: &MPhase) -> bool {
    // Chief.lean:320
    p.st == "queued" && (p.door == "two" || p.token == Some(r))
}

fn quiet_bars(p: &MPhase) -> bool {
    // Chief.lean:323
    p.door == "one" || p.kind == "act"
}

fn f_dispatch(r: i64, p: &MPhase) -> MPhase {
    // Chief.lean:326
    if !can_go(r, p) {
        return p.clone();
    }
    let one = p.door == "one";
    MPhase {
        st: "running",
        token: None,
        attempts: p.attempts + 1,
        ow_acts: p.ow_acts + i64::from(one),
        stale: p.stale || (one && p.last_yes != Some(r)),
        replay: p.replay || (one && p.doubt),
        ..p.clone()
    }
}

fn f_result(good: bool, p: &MPhase) -> MPhase {
    // Chief.lean:334
    if p.st == "running" {
        MPhase { st: if good { "ok" } else { "failed" }, ..p.clone() }
    } else {
        p.clone()
    }
}

fn f_reflect(retry: bool, p: &MPhase) -> MPhase {
    // Chief.lean:339
    if p.st != "failed" {
        return p.clone();
    }
    if p.door == "one" {
        return MPhase { st: "queued", ..p.clone() };
    }
    if retry && p.retries < KK.max_retries {
        return MPhase { st: "queued", retries: p.retries + 1, ..p.clone() };
    }
    p.clone()
}

fn f_requeue(p: &MPhase) -> MPhase {
    // Chief.lean:346
    if p.st == "ok" || p.st == "failed" {
        MPhase { st: "queued", ..p.clone() }
    } else {
        p.clone()
    }
}

fn f_restart(p: &MPhase) -> MPhase {
    // Chief.lean:350
    if p.st == "running" {
        return if p.door == "one" {
            MPhase { st: "doubt", doubt: true, ..p.clone() }
        } else {
            MPhase { st: "queued", ..p.clone() }
        };
    }
    if p.st == "asked" {
        return MPhase { st: "queued", ask_rev: None, ..p.clone() };
    }
    p.clone()
}

fn f_happened(p: &MPhase) -> MPhase {
    // Chief.lean:355
    if p.st == "doubt" {
        MPhase { st: "ok", ..p.clone() }
    } else {
        p.clone()
    }
}

fn f_didnt(p: &MPhase) -> MPhase {
    // Chief.lean:356
    if p.st == "doubt" {
        MPhase { st: "queued", ..p.clone() }
    } else {
        p.clone()
    }
}

fn fits(c: &MCard, k: Cost) -> bool {
    // Chief.lean:367
    c.spent.0 + c.held.0 + k.0 <= c.cap.0 && c.spent.1 + c.held.1 + k.1 <= c.cap.1
}

fn charge(c: &MCard, k: Cost) -> MCard {
    // Chief.lean:371
    MCard {
        running: None,
        held: ZERO,
        spent: (c.spent.0 + k.0, c.spent.1 + k.1),
        over: (c.over.0 + (k.0 - c.held.0).max(0), c.over.1 + (k.1 - c.held.1).max(0)),
        ..c.clone()
    }
}

fn on_phase(c: &MCard, n: i64, f: impl Fn(&MPhase) -> MPhase) -> MCard {
    // Chief.lean:378
    MCard { phases: upd(f, n, &c.phases), ..c.clone() }
}

fn all_confirmed(checks: &[(&str, bool)]) -> bool {
    // Chief.lean:380
    !checks.is_empty() && checks.iter().all(|(v, _)| *v == "confirmed")
}

type Ceil<'a> = &'a dyn Fn(&str) -> Cost;

fn card_step(vf: Vf, ceil: Ceil<'_>, q: bool, c: &MCard, e: &E) -> MCard {
    // Chief.lean:381
    let active = c.status == "active";
    match e {
        E::Ask(n) if active && !q => on_phase(c, *n, |p| f_ask(c.rev, p)),
        E::Answer(n, a) if active => on_phase(c, *n, |p| f_answer(c.rev, a, p)),
        E::GoId(n) if active => on_phase(c, *n, |p| f_go(c.rev, p)),
        E::Revise if active => MCard { rev: c.rev + 1, phases: c.phases.iter().map(f_clear).collect(), ..c.clone() },
        E::Dispatch(n) => {
            if let Some(p) = at_(&c.phases, *n) {
                if active && c.running.is_none() && can_go(c.rev, p) && fits(c, ceil(&p.kind)) && !(q && quiet_bars(p))
                {
                    return MCard {
                        phases: upd(|x| f_dispatch(c.rev, x), *n, &c.phases),
                        running: Some(*n as usize),
                        held: ceil(&p.kind),
                        ..c.clone()
                    };
                }
            }
            c.clone()
        }
        E::Result(n, good, k) => {
            if c.running.map(|r| r as i64) == Some(*n) {
                on_phase(&charge(c, *k), *n, |p| f_result(*good, p))
            } else {
                c.clone()
            }
        }
        E::Reflect(n, _, retry) => on_phase(c, *n, |p| f_reflect(*retry, p)),
        E::Requeue(n) if active => on_phase(c, *n, f_requeue),
        E::Restart(k) => {
            let c2 = if c.running.is_none() { c.clone() } else { charge(c, *k) };
            MCard { phases: c2.phases.iter().map(f_restart).collect(), ..c2 }
        }
        E::Happened(n) => on_phase(c, *n, f_happened),
        E::Didnt(n) => on_phase(c, *n, f_didnt),
        E::Oracle(n, ev) if active => {
            MCard { checks: upd(|_| (verdict(vf, ev), *ev == "observed"), *n, &c.checks), ..c.clone() }
        }
        E::Close if active && c.running.is_none() => {
            MCard { status: if all_confirmed(&c.checks) { "done" } else { "unverified" }, ..c.clone() }
        }
        E::OwnerDone if c.status == "active" || c.status == "unverified" => {
            MCard { status: "done", by_owner: true, ..c.clone() }
        }
        E::Reopen if c.status == "done" || c.status == "unverified" => MCard {
            status: "active",
            rev: c.rev + 1,
            checks: c.checks.iter().map(|_| ("pending", false)).collect(),
            by_owner: false,
            phases: c.phases.iter().map(f_clear).collect(),
            ..c.clone()
        },
        E::Drop if c.status != "dropped" => {
            MCard { status: "dropped", phases: c.phases.iter().map(f_clear).collect(), ..c.clone() }
        }
        E::RaiseBudget(k) => MCard { cap: (c.cap.0 + k.0, c.cap.1 + k.1), ..c.clone() },
        _ => c.clone(),
    }
}

fn interrupting(k: &str) -> bool {
    // Chief.lean:424
    k == "unprompted" || k == "reminder"
}

fn offer_m(d: &MDesk, k: &str) -> MDesk {
    // Chief.lean:432
    let route = |route: &'static str| MDesk { route, ..d.clone() };
    if k == "reply" || k == "receipt" {
        return route("now");
    }
    if k == "reminder" {
        return route(if !quiet(d.clock) && (d.at_break || d.waited) { "now" } else { "batch" });
    }
    if d.mode == "off" {
        return route("drop");
    }
    if d.mode == "shadow" {
        return route("silent");
    }
    if !quiet(d.clock) && d.at_break && d.pushes < KK.push_budget {
        return MDesk { route: "now", pushes: d.pushes + 1, ..d.clone() };
    }
    route("batch")
}

fn desk_step(d: &MDesk, e: &E) -> MDesk {
    // Chief.lean:444
    match e {
        E::Clock(m) => MDesk { clock: m.rem_euclid(1440), ..d.clone() },
        E::NewDay => MDesk { pushes: 0, ..d.clone() },
        E::Breakpoint => MDesk { at_break: true, ..d.clone() },
        E::Waited => MDesk { waited: true, ..d.clone() },
        E::Busy | E::Restart(_) => MDesk { at_break: false, ..d.clone() },
        E::Offer(k) => offer_m(d, k),
        _ => d.clone(),
    }
}

fn add_lesson(k: i64, xs: &[i64]) -> Vec<i64> {
    // Chief.lean:455
    let mut ys = xs.to_vec();
    ys.push(k);
    if KK.omega < ys.iter().filter(|x| **x == k).count() as i64 {
        let first = ys.iter().position(|x| *x == k).unwrap(); // List.erase: the first (oldest) one
        ys.remove(first);
    }
    ys
}

fn lesson_step(c: &MCard, xs: &[i64], e: &E) -> Vec<i64> {
    // Chief.lean:459
    if let E::Reflect(n, key, _) = e {
        if at_(&c.phases, *n).is_some_and(|p| p.st == "failed") {
            return add_lesson(*key, xs);
        }
    }
    xs.to_vec()
}

fn act_started(c: &MCard, c2: &MCard, e: &E) -> bool {
    // Chief.lean:468
    match e {
        E::Dispatch(n) => {
            let p = at_(&c.phases, *n);
            c2.running.map(|r| r as i64) == Some(*n) && c.running.is_none() && p.is_some_and(quiet_bars)
        }
        E::Ask(n) => {
            let (before, after) = (at_(&c.phases, *n), at_(&c2.phases, *n));
            after.is_some_and(|a| a.st == "asked") && before.is_none_or(|b| b.st != "asked")
        }
        _ => false,
    }
}

fn pushed_against_rule(d: &MDesk, d2: &MDesk, e: &E) -> bool {
    // Chief.lean:474
    let E::Offer(k) = e else { return false };
    d2.route == "now"
        && interrupting(k)
        && (quiet(d.clock) || (!d.at_break && !(*k == "reminder" && d.waited)) || (*k == "unprompted" && d.mode != "on"))
}

fn sent_now(d2: &MDesk, e: &E) -> i64 {
    // Chief.lean:479
    i64::from(matches!(e, E::Offer("unprompted")) && d2.route == "now")
}

fn step_with(vf: Vf, ceil: Ceil<'_>, s: &MS, e: &E) -> MS {
    // Chief.lean:483
    let q = quiet(s.desk.clock);
    let c2 = card_step(vf, ceil, q, &s.card, e);
    let d2 = desk_step(&s.desk, e);
    MS {
        lessons: lesson_step(&s.card, &s.lessons, e),
        quiet_act: s.quiet_act || (q && act_started(&s.card, &c2, e)),
        bad_push: s.bad_push || pushed_against_rule(&s.desk, &d2, e),
        sent_today: if matches!(e, E::NewDay) { 0 } else { s.sent_today + sent_now(&d2, e) },
        card: c2,
        desk: d2,
    }
}

fn run_model(ceil: Ceil<'_>, s: MS, events: &[E], vf: Vf) -> MS {
    events.iter().fold(s, |s, e| step_with(vf, ceil, &s, e))
}

fn done_needs_evidence(s: &MS) -> bool {
    // Chief.lean:498
    s.card.status != "done" || s.card.by_owner || (!s.card.checks.is_empty() && s.card.checks.iter().all(|(_, o)| *o))
}

fn spec(s: &MS) -> Vec<(&'static str, bool)> {
    // Chief.lean:498-515
    let (c, ps) = (&s.card, &s.card.phases);
    let k = &*KK;
    vec![
        ("DoneNeedsEvidence", done_needs_evidence(s)),
        ("OneYesOneAct", ps.iter().all(|p| p.ow_acts <= p.yeses && !p.stale)),
        ("BudgetBeforeDispatch", c.spent.0 + c.held.0 <= c.cap.0 + c.over.0 && c.spent.1 + c.held.1 <= c.cap.1 + c.over.1),
        (
            "PushBudgetQuiet",
            !s.bad_push && !s.quiet_act && s.sent_today <= k.push_budget && s.desk.pushes == s.sent_today,
        ),
        ("NoActReplayAfterRestart", ps.iter().all(|p| !p.replay)),
        (
            "OneWayNeverAutoRetried",
            ps.iter().all(|p| (p.door != "one" || (p.attempts <= p.yeses && p.retries == 0)) && p.retries <= k.max_retries),
        ),
        ("LessonsBounded", s.lessons.iter().all(|x| s.lessons.iter().filter(|y| *y == x).count() as i64 <= k.omega)),
    ]
}

fn broken(s: &MS) -> Vec<&'static str> {
    spec(s).into_iter().filter(|(_, v)| !v).map(|(k, _)| k).collect()
}

// ---- the transliteration against Lean's own worked traces (Chief.lean:975-1050) --------------------------------

fn ceil0(_: &str) -> Cost {
    // Chief.lean:984
    (10, 600)
}

const ORDER_DESK: [(&str, &str); 3] = [("research", "two"), ("assess", "two"), ("act", "one")]; // Chief.lean:987

fn order_desk(mode: &'static str, phases: Option<&[(&str, &'static str)]>, checks: usize) -> MS {
    start(phases.unwrap_or(&ORDER_DESK), checks, (100, 1800), mode)
}

fn desk3() -> MS {
    order_desk("shadow", None, 3)
}

fn ans(n: i64, a: &str) -> E {
    E::Answer(n, a.to_string())
}

#[test]
fn test_the_transliteration_reproduces_every_worked_trace_lean_proved() {
    use E::*;
    let v = Vf::Verdict;
    let routine = order_desk("shadow", Some(&[("act", "two")]), 1);
    let dangerous = order_desk("shadow", Some(&[("act", "one")]), 1);
    assert_eq!(run_model(&ceil0, routine, &[Clock(600), Dispatch(0)], v).card.running, Some(0));
    assert_eq!(run_model(&ceil0, dangerous, &[Clock(600), Dispatch(0)], v).card.running, None);
    let one: &[(&str, &str)] = &[("research", "two")];
    let read = [Clock(600), Dispatch(0), Result(0, true, (3, 60)), Oracle(0, "executorText"), Close];
    let naive = run_model(&ceil0, order_desk("shadow", Some(one), 1), &read, Vf::Naive);
    assert!(naive.card.status == "done" && !done_needs_evidence(&naive)); // naive_violates
    let word = run_model(&ceil0, order_desk("shadow", Some(one), 1), &read, v);
    assert_eq!(word.card.status, "unverified"); // executor_word_is_unverified
    let job = run_model(
        &ceil0,
        desk3(),
        &[
            Clock(600),
            Dispatch(0),
            Result(0, true, (3, 90)),
            Oracle(0, "observed"),
            Dispatch(1),
            Result(1, true, (2, 20)),
            Oracle(1, "observed"),
            Ask(2),
            ans(2, "yes"),
            Dispatch(2),
            Result(2, true, (0, 500)),
            Oracle(2, "observed"),
            Close,
        ],
        v,
    );
    assert!(job.card.status == "done" && job.card.phases.iter().map(|p| p.attempts).collect::<Vec<_>>() == [1, 1, 1]);
    assert_eq!(job.card.spent, (5, 610)); // desk_job_closes
    let r = |events: &[E]| run_model(&ceil0, desk3(), events, v);
    assert_eq!(r(&[Clock(600), Ask(2), ans(2, "timeout"), Dispatch(2)]).card.running, None); // timeout_is_no
    assert_eq!(r(&[Clock(600), Ask(2), ans(2, "yes"), Revise, Dispatch(2)]).card.running, None); // revision_cancels
    let pre = vec![Clock(600), Ask(2), ans(2, "yes"), Dispatch(2), Restart((0, 30)), Didnt(2), Dispatch(2)];
    assert_eq!(r(&pre).card.running, None); // restart_needs_a_new_yes
    let mut again = pre.clone();
    again.extend([Ask(2), ans(2, "yes"), Dispatch(2)]);
    assert_eq!(r(&again).card.running, Some(2));
    assert_eq!(
        r(&[Clock(600), Ask(2), ans(2, "yes"), Dispatch(2), Result(2, false, (0, 30)), Reflect(2, 7, true), Dispatch(2)])
            .card
            .running,
        None
    ); // one_way_failure_waits_for_a_go
    let mut tries = vec![Clock(600)];
    for _ in 0..3 {
        tries.extend([Dispatch(0), Result(0, false, (1, 10)), Reflect(0, 5, true)]);
    }
    tries.extend([Dispatch(0), Reflect(0, 5, true)]);
    let three = run_model(&|_: &str| (1, 10), desk3(), &tries, v);
    assert!(three.card.phases[0].attempts == 3 && three.card.running.is_none() && three.lessons == [5, 5, 5]); // two_way_three_trials
    let pushed = run_model(
        &ceil0,
        order_desk("on", None, 3),
        &[Clock(600), Breakpoint, Offer("unprompted"), Offer("unprompted"), Restart((0, 0)), Breakpoint, Offer("unprompted")],
        v,
    );
    assert!(pushed.desk.route == "batch" && pushed.desk.pushes == 2); // third_push_is_batched
    assert_eq!(r(&[Clock(1380), Breakpoint, Offer("reminder")]).desk.route, "batch");
    assert_eq!(r(&[Clock(1185), Waited, Offer("reminder")]).desk.route, "now");
    assert_eq!(r(&[Clock(1140), Offer("reminder")]).desk.route, "batch");
    assert_eq!(r(&[Clock(1380), Waited, Offer("reminder")]).desk.route, "batch"); // waited_reminder_goes
    assert_eq!(r(&[Clock(600), Ask(2), ans(2, "yes"), Clock(1380), Dispatch(2)]).card.running, None); // no_act_at_night
    for s in [&job, &word, &three, &pushed] {
        assert!(broken(s).is_empty(), "{:?}", broken(s));
    }
    assert!(broken(&naive).contains(&"DoneNeedsEvidence"));
}

// ---- the replay against the chief ----------------------------------------------------------------------------

fn micro(usd: f64) -> i64 {
    py::round_ndigits(usd * 1e6, 3).ceil() as i64
}

fn secs_of(s: f64) -> i64 {
    py::round_ndigits(s, 3).ceil() as i64
}

/// CEILINGS in the model's units (the web read's for research: the fakes' router always says web).
fn ceilings(kind: &str) -> Cost {
    let c = CEILINGS.iter().find(|(k, _)| *k == kind).unwrap().1;
    (micro(c.usd), secs_of(c.secs))
}

/// An act's (final sentence, UI states, ok) for each outcome the fakes produce.
fn ui_final(outcome: &str) -> (&'static str, Vec<Value>, bool) {
    match outcome {
        "good" => ("I ordered it.", vec![json!({"state": "Thank you! Order confirmed #123"})], true),
        "word" => ("I placed the order. Order confirmed.", vec![], true),
        "bad" => ("The payment did not go through.", vec![json!({"state": "Payment declined"})], false),
        _ => panic!("{outcome}"),
    }
}

async fn settle() {
    for _ in 0..60 {
        tokio::task::yield_now().await;
    }
}

/// The variant: the act adapter takes the executor's own sentence for a screen state.
fn trusting(final_text: &str, ui_evidence: &[Value], run: &eve_chief_receipt::AgentRun) -> PhaseResult {
    let mut ui = vec![json!({"state": final_text})];
    ui.extend(ui_evidence.iter().cloned());
    eve_chief_receipt::from_agent(final_text, &ui, run)
}

/// The fakes' shared state: what each executor is holding, and the reflection's word.
#[derive(Default)]
struct Held {
    research: Option<oneshot::Sender<Value>>,
    assess: Option<oneshot::Sender<Value>>,
    ask: Option<oneshot::Sender<String>>,
    act_running: bool,
    retry_word: bool,
    reflect_error: bool,
}

type R = std::result::Result<(), String>;

/// One card (the standing-desk job of design 4.1) driven through the real Chief, and the model beside it.
struct Replay {
    folder: PathBuf,
    vf: Vf,
    check_spec: bool,
    trust: bool,
    t: Arc<Mutex<NaiveDateTime>>,
    held: Arc<Mutex<Held>>,
    /// what each phase last returned
    latest: [Option<&'static str>; 4],
    keys: IndexMap<(String, String), i64>,
    seen: usize,
    /// revisions the code made on a card the model does not revise (doc 5)
    rev_offset: i64,
    fed: Vec<E>,
    stats: HashMap<String, usize>,
    chief: Arc<Chief>,
    m: MS,
}

impl Replay {
    fn new(folder: &Path, vf: Vf, check_spec: bool, trust: bool) -> Replay {
        let t = Arc::new(Mutex::new(day0() + TimeDelta::hours(10)));
        let held = Arc::new(Mutex::new(Held { retry_word: true, ..Held::default() }));
        let chief = Self::make(folder, &t, &held, trust);
        Replay {
            folder: folder.to_path_buf(),
            vf,
            check_spec,
            trust,
            t,
            held,
            latest: [None; 4],
            keys: IndexMap::new(),
            seen: 0,
            rev_offset: 0,
            fed: Vec::new(),
            stats: HashMap::new(),
            chief,
            m: order_desk("shadow", None, 3),
        }
    }

    fn stat(&mut self, key: &str) {
        *self.stats.entry(key.to_string()).or_default() += 1;
    }

    // -- the fakes --
    fn make(folder: &Path, t: &Arc<Mutex<NaiveDateTime>>, held: &Arc<Mutex<Held>>, trust: bool) -> Arc<Chief> {
        let tc = t.clone();
        let clock: Clock = Arc::new(move || local_ts(*tc.lock()));
        // this card's spend rows in its own folder: every replay's card is c1, and resume reads rows by id and date
        let spend = eve_spend::Ledger::at(folder.join("spend"));
        let h = held.clone();
        let reflect_ask: eve_chief_reflect::Ask = Arc::new(move |_body: &Value| {
            let h = h.lock();
            if h.reflect_error {
                return Err(AskError::new("RuntimeError", "the reflection model is down"));
            }
            let lesson = json!({"lesson": "Use the store's own search with the price cap.", "retry": h.retry_word});
            Ok(json!({"choices": [{"message": {"content": lesson.to_string()}}],
                      "usage": {"prompt_tokens": 300, "completion_tokens": 40, "cost": 0.0}}))
        });
        let c = clock.clone();
        let memory = Memory::new(Some(folder), Some(Arc::new(move || c())));
        let c = clock.clone();
        let reflector = Reflector::new(
            Some(memory),
            ReflectorOptions {
                ask: Some(reflect_ask),
                clock: Arc::new(move || c()),
                offload: false,
                spend: spend.clone(),
                ..ReflectorOptions::default()
            },
        );
        let h = held.clone();
        let research: Research = Arc::new(move |_q: String| {
            let (tx, rx) = oneshot::channel();
            h.lock().research = Some(tx);
            Box::pin(async move { rx.await.map_err(|_| Raised::new("CancelledError", "")) })
        });
        let h = held.clone();
        let create: Create = Arc::new(move |_payload: Value| {
            let (tx, rx) = oneshot::channel();
            h.lock().assess = Some(tx);
            Box::pin(async move { rx.await.map_err(|_| Raised::new("CancelledError", "")) })
        });
        let h = held.clone();
        let ask: Ask = Arc::new(move |_text: String| {
            let (tx, rx) = oneshot::channel();
            h.lock().ask = Some(tx);
            Box::pin(async move { rx.await.map_err(|_| Raised::new("CancelledError", "")) })
        });
        let h = held.clone();
        let start_task: StartTask = Arc::new(move |args: StartArgs| {
            assert_eq!(args.floor.as_deref(), Some("codex"), "an act runs where it can stop and ask");
            h.lock().act_running = true;
            Box::pin(async { Ok(json!({"ok": true})) })
        });
        let notify: Notify = Arc::new(|_t: String, _a: String| Box::pin(async { Ok(true) }));
        let chief = Chief::new(ChiefOptions {
            ledger: Some(Ledger::with_clock(Some(folder), clock.clone())),
            desk: Some(Desk::new(
                Some(folder),
                DeskOptions {
                    mode: Some("shadow".into()),
                    clock: Some(clock.clone()),
                    environ: Some(HashMap::new()),
                    ..DeskOptions::default()
                },
            )),
            reflector: Some(Arc::new(reflector)),
            research: Some(research),
            route_research: Some(Arc::new(|_goal: &str| Ok("web".to_string()))),
            start_task: Some(start_task),
            mac_free: Some(Arc::new(|| Ok(true))),
            create: Some(create),
            ask: Some(ask),
            notify: Some(notify),
            clock: Some(clock),
            offload: false,
            home: Some(folder.to_path_buf()),
            // Chief.lean has no replan step; the replanner's rules are replayed in the replan tests
            environ: Some(env(&[("EVE_CHIEF_REPLAN", "off")])),
            spend,
            ..ChiefOptions::default()
        });
        if trust {
            *chief.from_agent.lock() = Some(trusting);
        }
        chief
    }

    // -- reading the code --
    fn card(&self) -> Card {
        self.chief.ledger.get("c1").unwrap()
    }

    fn events(&self) -> Vec<Map<String, Value>> {
        self.chief.ledger.events(Some("c1"))
    }

    fn count(&self, kind: &str, n: usize) -> usize {
        self.events().iter().filter(|e| e["e"] == kind && e.get("phase") == Some(&json!(n))).count()
    }

    fn ask_out(&self) -> bool {
        self.held.lock().ask.as_ref().is_some_and(|tx| !tx.is_closed())
    }

    /// A Go for phase n is out at this revision and nothing answered it yet.
    fn asking(&self, n: usize, rev: i64) -> bool {
        if !self.ask_out() {
            return false;
        }
        let mut last: Option<Map<String, Value>> = None;
        for e in self.events() {
            if e["e"] == "asked" {
                last = Some(e);
            } else if e["e"] == "approved" && last.as_ref().is_some_and(|l| e.get("phase") == l.get("phase")) {
                last = None;
            }
        }
        last.is_some_and(|l| l.get("phase") == Some(&json!(n)) && l.get("rev") == Some(&json!(rev)))
    }

    /// What the oracle for this check could read, from what the fakes returned (the test wrote it).
    fn truth(&self, check: i64) -> &'static str {
        let (research, assess, act) = (self.latest[1], self.latest[2], self.latest[3]);
        match check {
            0 => match research {
                None => "missing",
                Some("good") => "observed",
                Some(_) => "contradicted",
            },
            1 => match assess {
                None => "missing",
                Some("good") if research == Some("good") => "observed",
                Some(_) => "contradicted",
            },
            _ => match act {
                None | Some("owner") => "missing",
                Some("good") => "observed",
                Some("bad") => "contradicted",
                Some("word") => "executorText",
                Some(other) => panic!("{other}"),
            },
        }
    }

    // -- the model --
    fn feed(&mut self, e: E) -> R {
        self.m = step_with(self.vf, &ceilings, &self.m, &e);
        self.fed.push(e.clone());
        if self.check_spec {
            let broken = broken(&self.m);
            self.expect(broken.is_empty(), &format!("the model's Spec fails after {e:?}: {broken:?}"))?;
        }
        Ok(())
    }

    fn expect(&self, ok: bool, why: &str) -> R {
        if ok {
            Ok(())
        } else {
            Err(format!("{why}\nmodel events so far: {:?}", self.fed))
        }
    }

    fn would_dispatch(&self) -> Option<i64> {
        let c = &self.m.card;
        let cand = c.phases.iter().position(|p| p.st != "ok")? as i64;
        let after = card_step(self.vf, &ceilings, quiet(self.m.desk.clock), c, &E::Dispatch(cand));
        (after.running.map(|r| r as i64) == Some(cand) && c.running.is_none()).then_some(cand)
    }

    fn charged(&self) -> std::result::Result<Cost, String> {
        let card = self.card();
        let k = (micro(card.spent.usd) - self.m.card.spent.0, secs_of(card.spent.wall_s) - self.m.card.spent.1);
        self.expect(k.0 >= 0 && k.1 >= 0, &format!("the code's spend went down: {:?}", card.spent))?;
        Ok(k)
    }

    /// Every ledger line the code wrote since the last sync, as the model's events, in order.
    fn sync(&mut self) -> R {
        let events = self.events();
        let new: Vec<Map<String, Value>> = events[self.seen..].to_vec();
        self.seen = events.len();
        let mut owner_close = false;
        for e in new {
            let kind = py::str_value(&e["e"]);
            let n = e.get("phase").and_then(Value::as_i64);
            let i = n.map(|n| n - 1).unwrap_or(-1);
            let shown = n.map(|n| n.to_string()).unwrap_or_else(|| "None".into());
            match kind.as_str() {
                "dispatched" => {
                    self.feed(E::Dispatch(i))?;
                    self.expect(
                        self.m.card.running.map(|r| r as i64) == Some(i),
                        &format!("the code started step {shown}; the model does not"),
                    )?;
                    let k = self.m.card.phases[i as usize].kind.clone();
                    self.stat(&format!("dispatch_{k}"));
                }
                "asked" => {
                    self.feed(E::Ask(i))?;
                    self.expect(
                        self.m.card.phases[i as usize].st == "asked",
                        &format!("the code asked a Go for step {shown}; the model does not (quiet: {})", quiet(self.m.desk.clock)),
                    )?;
                    self.stat("asked");
                }
                "answered" => {
                    let a = py::str_value(&e["answer"]);
                    if a == "happened" {
                        self.feed(E::Happened(i))?;
                        self.stat("happened");
                    } else if a == "didnt" {
                        self.feed(E::Didnt(i))?;
                        self.stat("didnt");
                    } else {
                        self.feed(E::Answer(i, a.clone()))?;
                        self.stat(&format!("answer_{a}"));
                    }
                }
                "approved" => {
                    let st = self.m.card.phases[i as usize].st;
                    if e.get("via") == Some(&json!("id")) {
                        if st == "doubt" {
                            // "go c1" on an act in doubt: It didn't, then go (doc 2)
                            self.feed(E::Didnt(i))?;
                            self.stat("go_from_doubt");
                        }
                        self.feed(E::GoId(i))?;
                        self.stat("go_by_id");
                    }
                    let via = py::str_value(e.get("via").unwrap_or(&Value::Null));
                    self.expect(
                        self.m.card.phases[i as usize].token == Some(self.m.card.rev),
                        &format!("the code recorded a yes for step {shown} ({via}); the model grants none"),
                    )?;
                }
                "result" if e.get("by") != Some(&json!("owner")) => {
                    self.expect(e.get("started") != Some(&json!(false)), &format!("a step did not start: {e:?}"))?;
                    let stale = py::truthy(e.get("stale"));
                    let cost = if stale { ZERO } else { self.charged()? };
                    self.feed(E::Result(i, e.get("status") == Some(&json!("done")), cost))?;
                    let key = if stale { "stale_result".to_string() } else { format!("result_{}", py::str_value(&e["status"])) };
                    self.stat(&key);
                }
                "checked" => {
                    if e.get("by") == Some(&json!("owner")) {
                        self.feed(E::OwnerDone)?;
                        owner_close = true;
                        continue;
                    }
                    let ci = e["check"].as_i64().unwrap();
                    let ev = self.truth(ci);
                    self.feed(E::Oracle(ci, ev))?;
                    let want = verdict(self.vf, ev);
                    self.expect(
                        e.get("outcome") == Some(&json!(want)),
                        &format!(
                            "check {ci} ({}): the code says {}, the model {want} on {ev}",
                            py::str_value(e.get("kind").unwrap_or(&Value::Null)),
                            py::str_value(e.get("outcome").unwrap_or(&Value::Null))
                        ),
                    )?;
                    self.stat(&format!("check_{ev}"));
                }
                "reflected" => {
                    let k = (
                        py::str_value(e.get("executor").unwrap_or(&Value::Null)),
                        py::str_value(e.get("do").unwrap_or(&Value::Null)),
                    );
                    let next = self.keys.len() as i64;
                    let key = *self.keys.entry(k.clone()).or_insert(next);
                    let retry = self.held.lock().retry_word && fits(&self.m.card, ceilings(&k.1)); // doc 3
                    self.feed(E::Reflect(i, key, retry))?;
                    self.stat("reflected");
                }
                "retried" => self.stat("retried"),
                "revised" => {
                    if self.m.card.status != "active" {
                        // "change c1" on an unverified card (doc 5)
                        self.rev_offset += 1;
                        self.stat("revised_closed");
                    }
                    self.feed(E::Revise)?;
                }
                "reopened" => {
                    self.feed(E::Reopen)?;
                    self.stat("reopened");
                }
                "state" => {
                    let why = py::str_value(e.get("why").unwrap_or(&Value::Null));
                    if why == "closed" {
                        if owner_close {
                            owner_close = false;
                        } else {
                            self.feed(E::Close)?;
                        }
                        let st = self.m.card.status;
                        self.stat(&format!("closed_{st}"));
                    } else if why == "dropped by the owner" {
                        self.feed(E::Drop)?;
                        self.stat("dropped");
                    } else if why == "reopened" && n.is_some() && e.get("phase_status") == Some(&json!("queued")) {
                        self.feed(E::Requeue(i))?;
                    } else if why == "budget raised by the owner" {
                        let card = self.card();
                        let k = (micro(card.budget.usd) - self.m.card.cap.0, card.budget.wall_min * 60 - self.m.card.cap.1);
                        self.feed(E::RaiseBudget(k))?;
                        self.stat("raised");
                    } else if why == "over budget" {
                        self.stat("over_budget");
                    } else if why.ends_with("may have done it") {
                        self.stat("doubt");
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// The projection both sides must agree on.
    fn compare(&self) -> R {
        let card = self.card();
        let m = &self.m.card;
        self.expect(
            m.rev + self.rev_offset == card.rev,
            &format!("revision: code {}, model {} (+{})", card.rev, m.rev, self.rev_offset),
        )?;
        let status = if card.status == "waiting" { "active" } else { card.status.as_str() };
        self.expect(m.status == status, &format!("status: code {}, model {}", card.status, m.status))?;
        for p in &card.phases {
            let mp = &m.phases[p.n - 1];
            let (dispatches, retries) = (self.count("dispatched", p.n), self.count("retried", p.n));
            self.expect(
                mp.attempts == dispatches as i64,
                &format!("step {} attempts: code {dispatches}, model {}", p.n, mp.attempts),
            )?;
            self.expect(mp.retries == retries as i64, &format!("step {} retries: code {retries}, model {}", p.n, mp.retries))?;
        }
        if status == "active" {
            let running: Vec<usize> = card.phases.iter().filter(|p| p.status == "running").map(|p| p.n - 1).collect();
            self.expect(running.first().copied() == m.running, &format!("running: code {running:?}, model {:?}", m.running))?;
            let doubtful = self.chief.doubtful(&card);
            for p in &card.phases {
                let mp = &m.phases[p.n - 1];
                let code = if p.status == "queued" {
                    let in_doubt = doubtful.as_ref().is_some_and(|d| d.n == p.n) && p.approval.is_none(); // doc 2
                    if in_doubt {
                        "doubt"
                    } else if self.asking(p.n, card.rev) {
                        "asked"
                    } else {
                        "queued"
                    }
                } else {
                    match p.status.as_str() {
                        "refused" => "failed",
                        "skipped" => "queued",
                        other => other,
                    }
                };
                let same = code == mp.st || (code == "failed" && mp.st == "queued" && mp.door == "one"); // doc 1
                self.expect(same, &format!("step {}: code {} ({code}), model {}", p.n, p.status, mp.st))?;
                let has = p
                    .approval
                    .as_ref()
                    .is_some_and(|a| a.rev == card.rev && !self.chief.st.lock().used.contains(&a.token));
                self.expect(has == mp.token.is_some(), &format!("step {} yes: code {has}, model {:?}", p.n, mp.token))?;
            }
        }
        let kept: HashMap<(String, String), usize> =
            self.chief.reflector.memory().count().into_iter().filter(|(_, v)| *v > 0).collect();
        let model: HashMap<(String, String), usize> = self
            .keys
            .iter()
            .map(|(k, i)| (k.clone(), self.m.lessons.iter().filter(|x| *x == i).count()))
            .filter(|(_, v)| *v > 0)
            .collect();
        self.expect(kept == model, &format!("lessons per key: code {kept:?}, model {model:?}"))
    }

    // -- the moves --
    async fn begin(&mut self) -> R {
        let out = self.chief.handle("take_on", &desk_job(), SAID, "2026-09-29 10:00", "telegram").await.unwrap();
        assert_eq!(out["ok"], true, "{out}");
        let card = self.card();
        let phases: Vec<(String, &'static str)> =
            card.phases.iter().map(|p| (p.r#do.clone(), if p.door == "one_way" { "one" } else { "two" })).collect();
        let phases: Vec<(&str, &'static str)> = phases.iter().map(|(k, d)| (k.as_str(), *d)).collect();
        self.m = start(&phases, card.done_checks.len(), (micro(card.budget.usd), card.budget.wall_min * 60), "shadow");
        let t = *self.t.lock();
        self.feed(E::Clock(i64::from(t.hour() * 60 + t.minute())))?;
        self.sync()?;
        self.compare()
    }

    async fn tick(&mut self) -> R {
        let would = self.would_dispatch();
        let dispatched = |r: &Replay| -> Vec<Value> {
            r.events().iter().filter(|e| e["e"] == "dispatched").map(|e| e["phase"].clone()).collect()
        };
        let before = dispatched(self).len();
        self.chief.tick().await.unwrap();
        settle().await;
        let started: Vec<Value> = dispatched(self)[before..].to_vec();
        self.sync()?;
        let want: Vec<Value> = would.map(|w| vec![json!(w + 1)]).unwrap_or_default();
        self.expect(started == want, &format!("dispatch: the code started {started:?}, the model would start {want:?}"))?;
        self.compare()
    }

    /// The executor the fakes are holding a step on, if any.
    fn running(&self) -> Option<&'static str> {
        let h = self.held.lock();
        if h.research.as_ref().is_some_and(|tx| !tx.is_closed()) {
            return Some("research");
        }
        if h.assess.as_ref().is_some_and(|tx| !tx.is_closed()) {
            return Some("assess");
        }
        h.act_running.then_some("act")
    }

    async fn finish(&mut self, outcome: &'static str, usd: f64, secs: f64, retry: bool, reflect_error: bool) -> R {
        let kind = self.running().expect("a step is held");
        let n = match kind {
            "research" => 1,
            "assess" => 2,
            _ => 3,
        };
        let card = self.card();
        let accepted = !card.terminal() && card.phase(n).is_some_and(|p| p.status == "running");
        {
            let mut h = self.held.lock();
            h.retry_word = retry;
            h.reflect_error = reflect_error;
        }
        if accepted {
            self.latest[n] = Some(outcome);
        }
        match kind {
            "research" => {
                let out = if outcome == "good" {
                    json!({"ok": true, "answer": "Desk A is $549; Desk B is $499.",
                           "sources": LINKS.iter().map(|u| json!({"url": u})).collect::<Vec<_>>(), "cost_usd": usd})
                } else {
                    json!({"ok": false, "reason": "the search failed"})
                };
                let tx = self.held.lock().research.take().unwrap();
                let _ = tx.send(out);
            }
            "assess" => {
                let text = if outcome == "good" {
                    json!({"options": [{"name": "Desk A", "price": 549, "why": "sturdy", "link": LINKS[0]}],
                           "pick": 0, "change": "a sale"})
                    .to_string()
                } else {
                    "not the options sheet".to_string()
                };
                let tx = self.held.lock().assess.take().unwrap();
                let _ = tx.send(crate::tests::reply_text(&text, json!({"input_tokens": 1000, "output_tokens": 200})));
            }
            _ => {
                self.held.lock().act_running = false;
                let (final_text, ui, ok) = ui_final(outcome);
                let end = AgentEnd { ok, secs, ..AgentEnd::default() };
                self.chief.on_agent_result("c1", 3, final_text, &ui, end).await.unwrap();
            }
        }
        settle().await;
        self.sync()?;
        self.compare()
    }

    async fn answer(&mut self, word: &str) -> R {
        let text = match word {
            "yes" => "Yes",
            "no" => "No",
            "timeout" => "no (no answer within 180 seconds)",
            _ => "maybe later",
        };
        let tx = self.held.lock().ask.take().expect("a Go is out");
        assert!(tx.send(text.to_string()).is_ok());
        settle().await;
        self.sync()?;
        self.compare()
    }

    /// An owner's code word or button, as the door calls it.
    async fn word(&mut self, name: &str) -> R {
        let c = self.chief.clone();
        match name {
            "go" => {
                c.approve("c1", "id", None, None, None).unwrap();
            }
            "revise" => {
                let title = format!("Standing desk r{}", self.card().rev + 1);
                let mut patch = Map::new();
                patch.insert("title".into(), json!(title));
                c.change("c1", &patch, "make it a standing desk").unwrap();
            }
            "drop" => {
                Chief::drop(&c, "c1").unwrap();
            }
            "done" => {
                c.done("c1").await.unwrap();
            }
            "reopen" => {
                c.reopen("c1").unwrap();
            }
            "happened" => {
                if c.happened("c1").await.unwrap()["ok"] == true {
                    self.latest[3] = Some("owner"); // the owner's word: a result, but no screen state
                }
            }
            "didnt" => {
                c.didnt("c1").unwrap();
            }
            "raise" => {
                c.raise_budget("c1").unwrap();
            }
            _ => panic!("{name}"),
        }
        settle().await;
        self.sync()?;
        self.compare()
    }

    /// The next time the clock reads `minute`: time never goes back (a held step waits for its time).
    async fn move_clock(&mut self, minute: i64) -> R {
        let now = *self.t.lock();
        let mut t = now.date().and_hms_opt((minute / 60) as u32, (minute % 60) as u32, now.second()).unwrap();
        if t <= now {
            t += TimeDelta::days(1);
        }
        if t.date() != now.date() {
            self.feed(E::NewDay)?;
        }
        *self.t.lock() = t;
        self.feed(E::Clock(minute))?;
        self.compare()
    }

    /// The daemon dies and launchd starts it again: the old chief's work is gone, a new one reads the folder.
    async fn restart(&mut self) -> R {
        let old = self.chief.clone();
        let tasks: Vec<JoinHandle<()>> = old.tasks.lock().drain(..).collect();
        for t in &tasks {
            t.abort();
        }
        for t in tasks {
            let _ = t.await;
        }
        {
            let mut h = self.held.lock();
            h.research = None;
            h.assess = None;
            h.ask = None;
            h.act_running = false;
        }
        self.chief = Self::make(&self.folder, &self.t, &self.held, self.trust);
        self.feed(E::Restart(ZERO))?;
        self.stat("restart");
        self.tick().await
    }
}

/// SplitMix64: a small seeded generator for the traces.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn random(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn choice<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[self.below(xs.len())]
    }
    fn weighted<T: Copy>(&mut self, xs: &[(T, u32)]) -> T {
        let total: u32 = xs.iter().map(|(_, w)| *w).sum();
        let mut r = (self.next() % u64::from(total)) as u32;
        for (x, w) in xs {
            if r < *w {
                return *x;
            }
            r -= w;
        }
        unreachable!()
    }
}

const MINUTES: [i64; 8] = [600, 1380, 30, 480, 479, 1349, 1350, 610];

async fn random_trace(r: &mut Replay, rng: &mut Rng, moves: usize) -> R {
    r.begin().await?;
    for _ in 0..moves {
        let card = r.card();
        let mut options: Vec<(&str, u32)> = vec![("tick", 30), ("restart", 3)];
        if r.running().is_some() {
            options.push(("finish", 30));
        }
        if r.ask_out() {
            options.push(("answer", 18));
        }
        if !card.terminal() {
            let raise = if card.waiting_for.as_deref() == Some("budget") { 4 } else { 1 };
            options.extend([("go", 4), ("revise", 2), ("drop", 1), ("done", 1), ("raise", raise)]);
            if r.chief.doubtful(&card).is_some_and(|d| d.approval.is_none()) {
                // doc 2
                options.extend([("happened", 5), ("didnt", 5)]);
            }
        }
        if (card.status == "done" || card.status == "unverified") && !card.phases.iter().any(|p| p.status == "running") {
            options.push(("reopen", 6));
        }
        if r.running() != Some("act") {
            options.push(("clock", 6));
        }
        match rng.weighted(&options) {
            "tick" => r.tick().await?,
            "restart" => r.restart().await?,
            "finish" => {
                let act = if r.running() == Some("act") { 15 } else { 0 };
                let outcome = rng.weighted(&[("good", 60), ("bad", 25), ("word", act)]);
                let usd = rng.choice(&[0.01, 0.2, 0.45]);
                let secs = rng.choice(&[30.0, 240.0, 700.0, 1300.0]);
                let (retry, reflect_error) = (rng.random() < 0.75, rng.random() < 0.1);
                r.finish(outcome, usd, secs, retry, reflect_error).await?
            }
            "answer" => {
                let a = rng.weighted(&[("yes", 70), ("no", 10), ("timeout", 10), ("hold", 10)]);
                r.answer(a).await?
            }
            "clock" => {
                let m = rng.choice(&MINUTES);
                r.move_clock(m).await?
            }
            word => r.word(word).await?,
        }
    }
    Ok(())
}

const SEEDS: u64 = 240;
const MOVES: usize = 40;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

/// Every seed's trace, run once for both tests below: (seed, the disagreement) for each that failed, and the
/// rules the traces reached.
static TRACES: OnceLock<(Vec<(u64, String)>, HashMap<String, usize>)> = OnceLock::new();

fn traces() -> &'static (Vec<(u64, String)>, HashMap<String, usize>) {
    TRACES.get_or_init(|| {
        let rt = runtime();
        let mut failed = Vec::new();
        let mut coverage: HashMap<String, usize> = HashMap::new();
        for seed in 0..SEEDS {
            let d = tempfile::tempdir().unwrap();
            let mut r = Replay::new(d.path(), Vf::Verdict, true, false);
            let mut rng = Rng(seed);
            if let Err(why) = rt.block_on(random_trace(&mut r, &mut rng, MOVES)) {
                failed.push((seed, why));
            }
            for (k, v) in &r.stats {
                *coverage.entry(k.clone()).or_default() += v;
            }
            // the old chief's tasks go with its runtime's next turn; nothing outlives the trace
            let tasks: Vec<JoinHandle<()>> = r.chief.tasks.lock().drain(..).collect();
            for t in tasks {
                t.abort();
            }
        }
        (failed, coverage)
    })
}

#[test]
fn test_generated_traces_the_code_and_the_model_agree() {
    let (failed, _) = traces();
    assert!(failed.is_empty(), "{} of {SEEDS} traces disagreed; the first: seed {} {}", failed.len(), failed[0].0, failed[0].1);
}

#[test]
fn test_the_generated_traces_reached_every_part_of_the_machine() {
    // A positive control on the generator: the traces above went through every rule they are meant to test.
    let (_, coverage) = traces();
    let need = [
        "dispatch_research", "dispatch_assess", "dispatch_act", "asked", "answer_yes", "answer_no", "answer_timeout",
        "answer_hold", "go_by_id", "result_done", "result_failed", "result_handed_on", "stale_result",
        "check_observed", "check_contradicted", "check_executorText", "check_missing", "reflected", "retried",
        "closed_done", "closed_unverified", "dropped", "reopened", "restart", "doubt", "happened", "didnt",
        "over_budget", "raised", "go_from_doubt", "revised_closed",
    ];
    let missing: Vec<&str> = need.iter().copied().filter(|k| coverage.get(*k).copied().unwrap_or(0) == 0).collect();
    assert!(missing.is_empty(), "{missing:?} {coverage:?}");
}

#[tokio::test]
async fn test_the_whole_job_of_design_4_1_agrees_step_by_step() {
    let d = tempfile::tempdir().unwrap();
    let mut r = Replay::new(d.path(), Vf::Verdict, true, false);
    async {
        r.begin().await?;
        r.tick().await?; // research
        r.finish("good", 0.01, 60.0, true, false).await?;
        r.tick().await?; // assess
        r.finish("good", 0.01, 60.0, true, false).await?;
        r.tick().await?; // the Go is asked
        r.answer("yes").await?;
        r.tick().await?; // the act, on codex
        r.finish("good", 0.01, 240.0, true, false).await?;
        r.tick().await // closes
    }
    .await
    .unwrap();
    assert!(r.card().status == "done" && r.m.card.status == "done" && !r.m.card.by_owner);
    assert_eq!(r.m.card.phases.iter().map(|p| p.attempts).collect::<Vec<_>>(), [1, 1, 1]);
}

#[tokio::test]
async fn test_night_holds_the_go_and_the_act_on_both_sides() {
    let d = tempfile::tempdir().unwrap();
    let mut r = Replay::new(d.path(), Vf::Verdict, true, false);
    r.begin().await.unwrap();
    r.tick().await.unwrap();
    r.finish("good", 0.01, 60.0, true, false).await.unwrap();
    r.tick().await.unwrap();
    r.finish("good", 0.01, 60.0, true, false).await.unwrap();
    r.move_clock(1380).await.unwrap(); // 23:00
    r.tick().await.unwrap(); // no Go asked
    assert!(r.held.lock().ask.is_none());
    r.move_clock(610).await.unwrap(); // 10:10 the next day
    r.tick().await.unwrap();
    r.answer("yes").await.unwrap();
    r.move_clock(1350).await.unwrap(); // 22:30: quiet again
    r.tick().await.unwrap(); // the approved act waits
    assert!(r.m.card.running.is_none() && !r.held.lock().act_running);
    r.move_clock(480).await.unwrap(); // 08:00
    r.tick().await.unwrap();
    assert!(r.held.lock().act_running && r.m.card.running == Some(2));
}

async fn executor_word_trace(r: &mut Replay) -> R {
    r.begin().await?;
    r.tick().await?;
    r.finish("good", 0.01, 60.0, true, false).await?;
    r.tick().await?;
    r.finish("good", 0.01, 60.0, true, false).await?;
    r.tick().await?;
    r.answer("yes").await?;
    r.tick().await?;
    r.finish("word", 0.01, 60.0, true, false).await?; // the executor says "Order confirmed."; no screen showed it
    r.tick().await
}

#[tokio::test]
async fn test_the_code_on_the_executors_word_agrees_the_card_is_unverified() {
    let d = tempfile::tempdir().unwrap();
    let mut r = Replay::new(d.path(), Vf::Verdict, true, false);
    executor_word_trace(&mut r).await.unwrap();
    assert!(r.card().status == "unverified" && r.m.card.status == "unverified");
}

#[tokio::test]
async fn test_positive_control_a_variant_that_closes_on_the_executors_word_disagrees() {
    let d = tempfile::tempdir().unwrap();
    let mut r = Replay::new(d.path(), Vf::Verdict, true, true);
    let why = executor_word_trace(&mut r).await.unwrap_err();
    assert!(why.contains("check 2 (ui_seen): the code says confirmed, the model unverifiable"), "{why}");
}

#[tokio::test]
async fn test_positive_control_the_variant_agrees_with_the_naive_oracle_and_breaks_done_needs_evidence() {
    // The same variant against Chief.lean's naiveVerdict: no disagreement, the card closes done, and the model's
    // end state breaks DoneNeedsEvidence, as naive_violates proves.
    let d = tempfile::tempdir().unwrap();
    let mut r = Replay::new(d.path(), Vf::Naive, false, true);
    executor_word_trace(&mut r).await.unwrap();
    assert!(r.card().status == "done" && r.m.card.status == "done");
    assert!(!done_needs_evidence(&r.m));
}

#[tokio::test]
async fn test_a_card_closed_during_a_web_step_then_reopened_moves_on() {
    let d = tempfile::tempdir().unwrap();
    let mut r = Replay::new(d.path(), Vf::Verdict, true, false);
    r.begin().await.unwrap();
    r.tick().await.unwrap(); // research starts
    r.word("done").await.unwrap(); // the owner closes it meanwhile
    r.finish("good", 0.01, 60.0, true, false).await.unwrap(); // the read ends: a stale result
    r.word("reopen").await.unwrap();
    r.tick().await.unwrap(); // the model runs assess; the code does too
}

// ---- the desk (PushBudgetQuiet) ------------------------------------------------------------------------------

fn desk_kinds(k: &str) -> &'static [&'static str] {
    match k {
        "unprompted" => &["warn", "close", "sheet"],
        "reminder" => &["reminder"],
        "reply" => &["reply"],
        _ => &["receipt"],
    }
}

/// Offers, breakpoints, busy spells, restarts, the clock and new days, through the model's `offer` and a
/// chief_desk Desk reloaded from the same folder at each offer (doc 6).
fn desk_replay(folder: &Path, mode: &'static str, rng: &mut Rng, moves: usize) -> HashMap<String, usize> {
    let mut s = order_desk(mode, None, 3);
    let mut day = day0();
    let mut minute: i64 = 600;
    s = step_with(Vf::Verdict, &ceil0, &s, &E::Clock(minute));
    let mut seen: HashMap<String, usize> = HashMap::new();
    let clocks: Vec<i64> = MINUTES.iter().copied().chain([1200, 900, 1439, 0]).collect();
    for _ in 0..moves {
        let mv = rng.weighted(&[("clock", 8), ("newDay", 3), ("breakpoint", 10), ("busy", 5), ("restart", 3), ("offer", 25)]);
        let e = match mv {
            "clock" => {
                minute = rng.choice(&clocks);
                E::Clock(minute)
            }
            "newDay" => {
                day += TimeDelta::days(1);
                E::NewDay
            }
            "offer" => E::Offer(rng.choice(&["unprompted", "reminder", "reply", "receipt"])),
            "breakpoint" => E::Breakpoint,
            "busy" => E::Busy,
            _ => E::Restart(ZERO),
        };
        let before = s.clone();
        s = step_with(Vf::Verdict, &ceil0, &s, &e);
        assert!(broken(&s).is_empty(), "{e:?} {:?}", broken(&s));
        let E::Offer(k) = e else { continue };
        let now = local_ts(day + TimeDelta::minutes(minute));
        let desk = Desk::new(
            Some(folder),
            DeskOptions {
                mode: Some(mode.into()),
                clock: Some(Arc::new(move || now)),
                environ: Some(HashMap::new()),
                ..DeskOptions::default()
            },
        );
        if before.desk.at_break {
            desk.breakpoint("task_end", Some(now)).unwrap();
        }
        let kind = rng.choice(desk_kinds(k));
        let item = Item { queued_at: now, ..Item::new(kind, "") };
        let route = desk.offer(&item, Some(now));
        let want = s.desk.route;
        *seen.entry(format!("{k}_{want}")).or_default() += 1;
        let at_break = before.desk.at_break;
        let where_ = format!("{mode} {k}/{kind} at {minute} (break {at_break}): code {route:?}, model {want}");
        assert_eq!(route.route == "now", want == "now", "{where_}");
        if k != "unprompted" || mode == "on" {
            assert_eq!(route.route, want, "{where_}");
        } else if mode == "off" {
            assert!(want == "drop" && (route.route, route.rule) == ("batch", "push_off"), "{where_}");
        } else {
            assert!((route.route == "silent" || route.route == "batch") && want == "silent", "{where_}");
        }
        let pushes = desk.pushes_today(Some(now)).unwrap() as i64;
        assert!(pushes <= KK.push_budget, "{where_}");
        if mode == "on" {
            assert!(pushes == s.desk.pushes && pushes == s.sent_today, "{where_} {pushes} {}", s.desk.pushes);
        }
    }
    seen
}

#[test]
fn test_the_desk_and_the_model_agree_on_every_offer() {
    for mode in ["off", "shadow", "on"] {
        for seed in 0..12 {
            let d = tempfile::tempdir().unwrap();
            let seen = desk_replay(d.path(), mode, &mut Rng(1000 + seed), 120);
            assert!(seen.values().sum::<usize>() > 0);
        }
    }
}

#[test]
fn test_the_desk_traces_reach_now_and_batch_for_each_interrupting_kind() {
    let mut seen: HashMap<String, usize> = HashMap::new();
    for seed in 0..12 {
        let d = tempfile::tempdir().unwrap();
        for (k, v) in desk_replay(d.path(), "on", &mut Rng(1000 + seed), 120) {
            *seen.entry(k).or_default() += v;
        }
    }
    for key in ["unprompted_now", "unprompted_batch", "reminder_now", "reminder_batch", "reply_now", "receipt_now"] {
        assert!(seen.get(key).copied().unwrap_or(0) > 0, "{key} {seen:?}");
    }
}
