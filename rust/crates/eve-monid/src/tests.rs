//! Port of bridge/tests/test_monid.py: the Monid CLI as the web search's last resort, with a fake runner (no
//! network, no real CLI). The websearch routing half of those tests (which engine failures reach Monid) belongs
//! to the eve-web crate; here every test drives `web_search` / `run` directly.

use super::*;
use std::io::Write as _;
use std::sync::Arc;

const QUERY: &str = "who is the current president of the united states";

/// The shape `monid run -p dataforseo -e /serp/google-organic -j` printed on 2026-10-08, trimmed.
fn run_output() -> Value {
    json!({
        "runId": "01TESTRUN", "provider": "dataforseo", "endpoint": "/serp/google-organic", "status": "COMPLETED",
        "output": [{
            "keyword": QUERY, "type": "organic", "items_count": 5,
            "items": [
                {"type": "ai_overview", "markdown": "The current president is Example Person, the 47th president.[[1]](https://example.org/a)\n- Name: Example"},
                {"type": "organic", "title": "President", "url": "https://example.org/a",
                 "description": "Example Person is the 47th and current president."},
                {"type": "people_also_ask", "items": []},
                {"type": "organic", "title": "The office", "url": "https://example.gov/b", "description": "About it."},
                {"type": "organic", "title": "dup", "url": "https://example.org/a", "description": "dup"},
            ]}],
        "billing": {"reportedCost": {"currency": "USD", "value": 2000, "unit": "MICRO_DOLLAR"}},
    })
}

fn keys() -> Value {
    json!([{"label": "main", "prefix": "<prefix>", "key": "<masked>", "added_at": "2026-10-08", "active": true}])
}

/// Answers `keys list`, `run` and `balance` like the 0.1.7 CLI; records each argv.
struct FakeCli {
    run_reply: Mutex<Value>,
    keys: Mutex<Value>,
    rc: i32,
    /// When set, `run` answers this code with no output (`keys list` still answers).
    run_rc: Mutex<Option<i32>>,
    calls: Mutex<Vec<Vec<String>>>,
    envs: Mutex<Vec<HashMap<String, String>>>,
    timeouts: Mutex<Vec<f64>>,
}

impl FakeCli {
    fn new() -> Self {
        Self {
            run_reply: Mutex::new(run_output()),
            keys: Mutex::new(keys()),
            rc: 0,
            run_rc: Mutex::new(None),
            calls: Mutex::new(Vec::new()),
            envs: Mutex::new(Vec::new()),
            timeouts: Mutex::new(Vec::new()),
        }
    }

    fn runs(&self) -> Vec<Vec<String>> {
        self.calls
            .lock()
            .iter()
            .filter(|a| a[1] == "run")
            .cloned()
            .collect()
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().clone()
    }
}

impl Runner for FakeCli {
    fn run(
        &self,
        argv: &[String],
        timeout: f64,
        env: &HashMap<String, String>,
    ) -> (i32, String, String) {
        self.calls.lock().push(argv.to_vec());
        self.envs.lock().push(env.clone());
        self.timeouts.lock().push(timeout);
        if argv[1] == "keys" {
            return (0, self.keys.lock().to_string(), String::new());
        }
        if argv[1] == "balance" {
            return (
                0,
                json!({"balance": {"value": 0.998, "currency": "USD"}, "held": {"value": 0}})
                    .to_string(),
                String::new(),
            );
        }
        if let Some(rc) = *self.run_rc.lock() {
            return (rc, String::new(), String::new());
        }
        let reply = self.run_reply.lock().clone();
        let text = match reply {
            Value::String(s) => s,
            other => other.to_string(),
        };
        (self.rc, text, String::new())
    }
}

/// The fixture: EVE_MONID=fallback, EVE_MONID_CLI=/fake/bin/monid, no caps set, the fake as the runner, and the
/// spend ledger (and so the count file) in a temp folder.
struct Fx {
    cli: Arc<FakeCli>,
    monid: Monid,
    env: Environ,
    dir: tempfile::TempDir,
}

fn base_env(dir: &Path) -> Environ {
    let mut env = Environ::new();
    env.insert("EVE_MONID".into(), "fallback".into());
    env.insert("EVE_MONID_CLI".into(), "/fake/bin/monid".into());
    env.insert("HOME".into(), dir.to_string_lossy().into_owned());
    env.insert("PATH".into(), "/usr/bin:/bin".into());
    env
}

fn fixture() -> Fx {
    fixture_with(|e| e)
}

fn fixture_with(edit: impl FnOnce(Environ) -> Environ) -> Fx {
    let dir = tempfile::tempdir().expect("tempdir");
    let env = edit(base_env(dir.path()));
    let cli = Arc::new(FakeCli::new());
    let monid = build(&cli, &env, dir.path());
    Fx {
        cli,
        monid,
        env,
        dir,
    }
}

fn build(cli: &Arc<FakeCli>, env: &Environ, dir: &Path) -> Monid {
    Monid::new()
        .with_runner(cli.clone())
        .with_ledger(Ledger::at(dir.join("spend")))
        .with_process_env(env.clone())
        .with_which(Arc::new(|_: &str| None))
}

fn search(fx: &Fx) -> Value {
    fx.monid.web_search(QUERY, 25.0, &SearchOpts::default())
}

fn spend_rows(dir: &Path) -> Vec<Value> {
    let mut rows = Vec::new();
    for entry in std::fs::read_dir(dir.join("spend")).expect("spend dir") {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            for line in std::fs::read_to_string(&path).expect("read").lines() {
                rows.push(serde_json::from_str(line).expect("row"));
            }
        }
    }
    rows
}

thread_local! {
    static CAPTURED: std::cell::RefCell<Option<Vec<u8>>> = const { std::cell::RefCell::new(None) };
}

/// Writes into this thread's capture buffer, when one is open.
struct ThreadBuf;

impl std::io::Write for ThreadBuf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        CAPTURED.with(|c| {
            if let Some(buf) = c.borrow_mut().as_mut() {
                buf.extend_from_slice(b);
            }
        });
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Every log line written while `f` runs, on this thread. One global subscriber (set once) writes into a
/// per-thread buffer: scoped dispatchers race on tracing's callsite interest cache when tests run in parallel.
fn capture<R>(f: impl FnOnce() -> R) -> (R, String) {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        let sub = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .with_writer(|| ThreadBuf)
            .finish();
        let _ = tracing::subscriber::set_global_default(sub);
    });
    CAPTURED.with(|c| *c.borrow_mut() = Some(Vec::new()));
    let out = f();
    let bytes = CAPTURED.with(|c| c.borrow_mut().take()).unwrap_or_default();
    (out, String::from_utf8_lossy(&bytes).into_owned())
}

fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-6 * b.abs().max(1e-12)
}

const SHAPE: [&str; 6] = ["ok", "answer", "sources", "usage", "ms", "cost_usd"];

#[test]
fn test_the_fallback_fires_only_when_both_engines_failed() {
    let fx = fixture();
    let out = search(&fx);
    assert_eq!(out["ok"], json!(true));
    assert_eq!(out["fallback"], json!("monid"));
    let mut keys: Vec<&str> = out
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort();
    let mut want: Vec<&str> = SHAPE.iter().copied().chain(["fallback"]).collect();
    want.sort();
    assert_eq!(keys, want);
    assert_eq!(
        out["sources"],
        json!([
            {"title": "President", "url": "https://example.org/a",
             "snippet": "Example Person is the 47th and current president."},
            {"title": "The office", "url": "https://example.gov/b", "snippet": "About it."}])
    );
    let answer = out["answer"].as_str().unwrap();
    assert!(answer.starts_with(
        "From Google results: The current president is Example Person, the 47th president. (Google AI overview) Example Person is the 47th"
    ));
    assert!(!answer.contains("[[1]]") && !answer.contains("Name:"));
    assert_eq!(out["cost_usd"], json!(0.002));
    assert_eq!(out["usage"], json!({"in": 0, "out": 0, "cost": 0.002}));
    assert!(out["ms"].is_i64());
    let runs = fx.cli.runs();
    assert_eq!(runs.len(), 1);
    let run = &runs[0];
    assert_eq!(
        &run[..5],
        &["/fake/bin/monid", "run", "-p", "dataforseo", "-e"]
    );
    assert_eq!(run.last().unwrap(), "-j");
    let i = run.iter().position(|a| a == "-i").unwrap();
    let body: Value = serde_json::from_str(&run[i + 1]).unwrap();
    assert_eq!(
        body,
        json!({"keyword": QUERY, "location_code": 2840, "language_code": "en", "depth": 10})
    );
    let w = run.iter().position(|a| a == "-w").unwrap();
    assert!(run[w + 1].parse::<i64>().unwrap() <= 12);
    assert!(*fx.cli.timeouts.lock().last().unwrap() <= 12.0 + 0.01);
    let env = fx.cli.envs.lock().last().unwrap().clone();
    assert_eq!(env["NO_COLOR"], "1");
    assert!(env["PATH"].split(':').any(|d| d == "/opt/homebrew/bin"));
}

#[test]
fn test_a_unitless_cost_is_unpriced_not_dollars() {
    assert_eq!(
        cost(&json!({"billing": {"reportedCost": {"value": 2000}}})),
        None
    );
    assert_eq!(
        cost(&json!({"billing": {"reportedCost": {"value": 2000, "unit": "MICRO_DOLLAR"}}})),
        Some(0.002)
    );
}

#[test]
fn test_odd_settings_never_raise() {
    let mut env = Environ::new();
    env.insert("EVE_MONID_PER_DAY".into(), "inf".into());
    env.insert("EVE_MONID_USD_PER_DAY".into(), "1e999".into());
    assert_eq!(per_day(Some(&env)), DEFAULT_PER_DAY);
    let usd = usd_per_day(Some(&env));
    assert!((0.0..=100.0).contains(&usd));
}

#[test]
fn test_the_cli_never_gets_eves_secrets() {
    let mut env = Environ::new();
    env.insert("OPENROUTER_API_KEY".into(), "sk-or-test".into());
    env.insert("EVE_TELEGRAM_TOKEN".into(), "123:abc".into());
    env.insert("HOME".into(), "/home/o".into());
    let m = Monid::new().with_process_env(env);
    let out = m.cli_env("/opt/homebrew/bin/monid");
    assert!(!out.contains_key("OPENROUTER_API_KEY") && !out.contains_key("EVE_TELEGRAM_TOKEN"));
    assert_eq!(out["NO_COLOR"], "1");
    assert!(out["PATH"].split(':').any(|d| d == "/opt/homebrew/bin"));
    assert_eq!(out["HOME"], "/home/o");
}

#[test]
fn test_the_cli_path_keeps_the_daemons_path_and_the_system_dirs() {
    // PATH was read from the new, minimal dict, which never has one: /usr/bin, /bin and nvm's node went, and the
    // CLI's `#!/usr/bin/env node` failed with 127
    let mut env = Environ::new();
    env.insert(
        "PATH".into(),
        "/home/o/.nvm/versions/node/v22/bin:/usr/bin:/bin".into(),
    );
    let out = Monid::new()
        .with_process_env(env)
        .cli_env("/opt/homebrew/bin/monid");
    let path: Vec<&str> = out["PATH"].split(':').collect();
    assert_eq!(path[0], "/opt/homebrew/bin");
    assert!(path.contains(&"/home/o/.nvm/versions/node/v22/bin"));
    assert!(path.contains(&"/usr/bin") && path.contains(&"/bin"));
    let unique: HashSet<&&str> = path.iter().collect();
    assert_eq!(unique.len(), path.len());
    let out = Monid::new()
        .with_process_env(Environ::new())
        .cli_env("/opt/homebrew/bin/monid");
    let path: Vec<&str> = out["PATH"].split(':').collect();
    assert!(path.contains(&"/usr/bin") && path.contains(&"/bin")); // a daemon started with no PATH still has env
}

#[test]
fn test_switched_off_never_runs() {
    let fx = fixture_with(|mut e| {
        e.insert("EVE_MONID".into(), "0".into());
        e
    });
    assert_eq!(
        search(&fx),
        json!({"ok": false, "reason": "Monid is switched off (EVE_MONID=0)"})
    );
    let out = fx
        .monid
        .run("p", "/e", &json!({}), 5.0, &RunOpts::default());
    assert_eq!(
        out,
        json!({"ok": false, "reason": "Monid is switched off (EVE_MONID=0)"})
    );
    assert!(fx.cli.calls().is_empty());
    for off in ["off", "FALSE", " no "] {
        let mut env = Environ::new();
        env.insert("EVE_MONID".into(), off.into());
        assert_eq!(mode(Some(&env)), "off");
    }
    let mut env = Environ::new();
    env.insert("EVE_MONID".into(), "yes".into());
    assert_eq!(mode(Some(&env)), "fallback");
    assert_eq!(mode(Some(&Environ::new())), "fallback");
}

#[test]
fn test_the_daily_run_cap() {
    let fx = fixture_with(|mut e| {
        e.insert("EVE_MONID_PER_DAY".into(), "2".into());
        e
    });
    assert_eq!(search(&fx)["ok"], json!(true));
    assert_eq!(search(&fx)["ok"], json!(true));
    let (out, logs) = capture(|| search(&fx));
    assert_eq!(
        out,
        json!({"ok": false, "reason": "today's 2 Monid runs are used up (EVE_MONID_PER_DAY)"})
    );
    assert_eq!(fx.cli.runs().len(), 2);
    let capped: Vec<&str> = logs.lines().filter(|l| l.contains("used up")).collect();
    assert_eq!(capped.len(), 1);
    assert!(capped[0].contains("EVE_MONID_PER_DAY"));
    // a restart reads the day's count back: no fresh cap
    let restarted = build(&fx.cli, &fx.env, fx.dir.path());
    assert_eq!(restarted.used_today(None).runs, 2);
    assert_eq!(
        restarted.web_search(QUERY, 25.0, &SearchOpts::default())["ok"],
        json!(false)
    );
    assert_eq!(fx.cli.runs().len(), 2);
}

#[test]
fn test_the_daily_dollar_cap() {
    let mut fx = fixture_with(|mut e| {
        e.insert("EVE_MONID_USD_PER_DAY".into(), "0.003".into());
        e
    });
    assert_eq!(search(&fx)["ok"], json!(true));
    let used = fx.monid.used_today(None);
    assert_eq!(used.runs, 1);
    assert!(approx(used.usd, 0.002));
    assert_eq!(search(&fx)["ok"], json!(false)); // 0.002 + 0.002 > 0.003
    assert_eq!(fx.cli.runs().len(), 1);
    fx.env.insert("EVE_MONID_USD_PER_DAY".into(), "0".into());
    let opts = RunOpts {
        environ: Some(fx.env.clone()),
        ..RunOpts::default()
    };
    assert_eq!(
        fx.monid.run("p", "/e", &json!({}), 5.0, &opts)["capped"],
        json!(true)
    ); // $0 a day stops them all
    fx.monid.reset_for_tests();
}

#[test]
fn test_a_reported_cost_replaces_the_estimate() {
    let fx = fixture();
    let mut reply = run_output();
    reply["billing"] = json!({"reportedCost": {"value": 1000, "unit": "MICRO_DOLLAR"}});
    *fx.cli.run_reply.lock() = reply;
    assert_eq!(
        fx.monid.web_search(QUERY, 12.0, &SearchOpts::default())["cost_usd"],
        json!(0.001)
    );
    assert!(approx(fx.monid.used_today(None).usd, 0.001));
}

#[test]
fn test_every_monid_failure_returns_the_original_reason() {
    let cases = [
        ("missing", "Monid is not set up on this computer"),
        ("no key", "Monid is not set up on this computer"),
        ("timeout", "monid did not answer in 12 s"),
        ("bad json", "monid sent unreadable JSON"),
        ("failed run", "the Monid run ended BLOCKED"),
        ("no results", "Monid found no results"),
        ("no time", "no time left for Monid"),
    ];
    for (case, reason) in cases {
        let mut budget = 25.0;
        let fx = if case == "missing" {
            let dir = tempfile::tempdir().unwrap();
            let mut env = base_env(dir.path());
            env.insert("EVE_MONID_CLI".into(), String::new());
            env.remove("NPM_CONFIG_PREFIX");
            let cli = Arc::new(FakeCli::new());
            let monid = build(&cli, &env, dir.path())
                .with_cli_paths(Vec::<String>::new())
                .with_npm_bin_dirs(Vec::<String>::new());
            Fx {
                cli,
                monid,
                env,
                dir,
            }
        } else {
            fixture()
        };
        match case {
            "no key" => *fx.cli.keys.lock() = json!([{"label": "main", "active": false}]),
            "timeout" => *fx.cli.run_rc.lock() = Some(RC_TIMEOUT),
            "bad json" => *fx.cli.run_reply.lock() = json!("Error: something {not json"),
            "failed run" => {
                let mut r = run_output();
                r["status"] = json!("BLOCKED");
                r["output"] = Value::Null;
                *fx.cli.run_reply.lock() = r;
            }
            "no results" => {
                let mut r = run_output();
                r["output"] = json!([{"items": [{"type": "related_searches"}]}]);
                *fx.cli.run_reply.lock() = r;
            }
            "no time" => budget = 2.0,
            _ => {}
        }
        let out = fx.monid.web_search(QUERY, budget, &SearchOpts::default());
        assert_eq!(out["ok"], json!(false), "{case}");
        if case == "no time" {
            assert!(fx.cli.runs().is_empty());
            assert_eq!(out["reason"], json!(reason));
            continue;
        }
        let got = out["reason"].as_str().unwrap();
        if case == "timeout" {
            // the wait is what is left of the 12 s after the key check: 12 s, or 11 s on a slow machine
            assert!(
                got.starts_with("monid did not answer in 1") && got.ends_with(" s"),
                "{case}: {got}"
            );
        } else {
            assert_eq!(got, reason, "{case}");
        }
        if case == "missing" || case == "no key" {
            assert!(fx.cli.runs().is_empty(), "{case}");
        }
    }
}

#[test]
fn test_a_run_is_recorded_in_the_spend_ledger() {
    let fx = fixture();
    assert_eq!(search(&fx)["ok"], json!(true));
    let rows: Vec<Value> = spend_rows(fx.dir.path())
        .into_iter()
        .filter(|r| r["p"] == "monid")
        .collect();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row["usd"], json!(0.002));
    assert_eq!(row["src"], json!("reported"));
    assert_eq!(row["f"], json!(eve_spend::SEARCH));
    assert_eq!(row["m"], json!("dataforseo/serp/google-organic"));
}

#[test]
fn test_an_unpriced_run_is_flagged_and_counted_at_its_list_price() {
    let fx = fixture();
    let mut reply = run_output();
    reply.as_object_mut().unwrap().shift_remove("billing");
    *fx.cli.run_reply.lock() = reply;
    assert_eq!(
        fx.monid.web_search(QUERY, 12.0, &SearchOpts::default())["cost_usd"],
        Value::Null
    );
    let rows: Vec<Value> = spend_rows(fx.dir.path())
        .into_iter()
        .filter(|r| r["p"] == "monid")
        .collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["usd"], Value::Null);
    assert_eq!(rows[0]["unpriced"], json!(true));
    assert_eq!(
        rows[0]["note"],
        json!("no reported cost (COMPLETED); ~$0.002 counted against the day")
    );
    assert!(approx(fx.monid.used_today(None).usd, SEARCH_USD));
}

#[test]
fn test_logs_carry_the_shape_never_the_query_or_the_key() {
    let fx = fixture();
    *fx.cli.keys.lock() = json!([{"label": "main", "key": "SECRET-SHAPED-VALUE", "active": true}]);
    let (out, text) = capture(|| search(&fx));
    assert_eq!(out["ok"], json!(true));
    assert!(
        text.contains("monid: search ok, 2 results in") && text.contains("$0.0020"),
        "{text:?}"
    );
    assert!(
        !text.contains(QUERY)
            && !text.contains("president")
            && !text.contains("SECRET-SHAPED-VALUE")
    );
}

#[test]
fn test_available_needs_the_cli_and_an_active_key() {
    let fx = fixture();
    assert!(fx.monid.available(None, None, KEYS_SECS));
    fx.monid.reset_for_tests();
    *fx.cli.keys.lock() = json!([]);
    assert!(!fx.monid.available(None, None, KEYS_SECS));
    *fx.cli.keys.lock() = json!({"items": [{"active": true}]});
    assert!(fx.monid.available(None, None, KEYS_SECS));
    // a found key is believed for a while: no second `keys list`
    let before = fx.cli.calls().len();
    assert!(fx.monid.available(None, None, KEYS_SECS));
    assert_eq!(fx.cli.calls().len(), before);
}

#[test]
fn test_balance() {
    let fx = fixture();
    assert_eq!(
        fx.monid.balance(None, None),
        json!({"ok": true, "usd": 0.998, "held_usd": 0.0})
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_the_async_wrappers_run_off_the_loop() {
    let fx = fixture();
    let m = Arc::new(fx.monid);
    let opts = RunOpts {
        estimate_usd: SEARCH_USD,
        ..RunOpts::default()
    };
    let out = m
        .clone()
        .arun(
            SEARCH_PROVIDER.into(),
            SEARCH_ENDPOINT.into(),
            search_body(QUERY),
            10.0,
            opts,
        )
        .await;
    let bal = m.clone().abalance(None, None).await;
    assert_eq!(out["ok"], json!(true));
    assert_eq!(out["cost_usd"], json!(0.002));
    assert_eq!(out["status"], json!("COMPLETED"));
    assert_eq!(bal["ok"], json!(true));
    // the module-level wrappers take the fake runner as an argument
    let runner: Arc<dyn Runner> = fx.cli.clone();
    let bal = abalance(Some(fx.env.clone()), Some(runner)).await;
    assert_eq!(bal["usd"], json!(0.998));
}

#[test]
fn test_the_real_runner_kills_on_timeout_and_reports_a_missing_binary() {
    let tmp = tempfile::tempdir().unwrap();
    let mut env = HashMap::new();
    env.insert("PATH".to_string(), "/bin".to_string());
    let t0 = Instant::now();
    let (rc, _, _) = subprocess_runner(&["/bin/sleep".into(), "5".into()], 0.2, &env);
    assert_eq!(rc, RC_TIMEOUT);
    assert!(t0.elapsed() < Duration::from_secs(3));
    let nope = tmp.path().join("nope").to_string_lossy().into_owned();
    let (rc, _, err) = subprocess_runner(&[nope], 1.0, &env);
    assert_eq!(rc, RC_NO_CLI);
    assert!(err.contains("could not start"), "{err}");
    assert_eq!(err, "monid could not start (No such file or directory)");
}

#[test]
fn test_the_cli_is_found_where_launchd_cannot_see_it() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = tmp.path().join("monid");
    std::fs::write(&fake, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let fake = fake.to_string_lossy().into_owned();
    let absent = tmp.path().join("absent").to_string_lossy().into_owned();
    let m = Monid::new()
        .with_process_env(base_env(tmp.path()))
        .with_which(Arc::new(|_: &str| None))
        .with_cli_paths([absent, fake.clone()]);
    assert_eq!(m.find_cli(Some(&Environ::new())), fake);
    let mut env = Environ::new();
    env.insert("EVE_MONID_CLI".into(), "/x/monid".into());
    assert_eq!(m.find_cli(Some(&env)), "/x/monid");
}

// ---- parity tests beyond the Python file ------------------------------------------------------------------------

#[test]
fn the_real_runner_returns_output_and_the_exit_code() {
    let mut env = HashMap::new();
    env.insert("PATH".to_string(), "/usr/bin:/bin".to_string());
    env.insert("NO_COLOR".to_string(), "1".to_string());
    let (rc, out, _) = subprocess_runner(
        &[
            "/bin/sh".into(),
            "-c".into(),
            "echo \"$NO_COLOR\"; echo oops >&2; exit 3".into(),
        ],
        5.0,
        &env,
    );
    assert_eq!((rc, out.as_str()), (3, "1\n"));
    // an argv[0] with no slash is looked up on the given PATH
    let (rc, out, err) =
        subprocess_runner(&["sh".into(), "-c".into(), "echo hi".into()], 5.0, &env);
    assert_eq!((rc, out.as_str(), err.as_str()), (0, "hi\n", ""));
}

#[test]
fn the_count_file_is_pythons_format_and_a_python_file_is_read_back() {
    let fx = fixture();
    let now = 1_790_000_000.0;
    let day = local_day(Some(now)).unwrap();
    assert!(fx.monid.take(SEARCH_USD, None, Some(now)).is_ok());
    let text = std::fs::read_to_string(fx.monid.count_path()).unwrap();
    assert_eq!(
        text,
        format!("{{\"day\": \"{day}\", \"runs\": 1, \"usd\": 0.002}}")
    );
    assert!(!fx.monid.count_path().with_extension("tmp").exists());
    // a file Python wrote (a later count) wins over this process's smaller one after a restart
    std::fs::write(
        fx.monid.count_path(),
        format!("{{\"day\": \"{day}\", \"runs\": 5, \"usd\": 0.01}}"),
    )
    .unwrap();
    let restarted = build(&fx.cli, &fx.env, fx.dir.path());
    assert_eq!(restarted.used_today(Some(now)), Used { runs: 5, usd: 0.01 });
    // another day's file is nothing saved; an unreadable one too
    let other = build(&fx.cli, &fx.env, fx.dir.path());
    assert_eq!(other.used_today(Some(now + 86400.0 * 3.0)), Used::default());
    let mut f = std::fs::File::create(fx.monid.count_path()).unwrap();
    f.write_all(b"{torn").unwrap();
    assert_eq!(
        build(&fx.cli, &fx.env, fx.dir.path()).used_today(Some(now)),
        Used::default()
    );
    // Python's int()/float() on the saved values: a numeric string counts, a float run count truncates
    std::fs::write(
        fx.monid.count_path(),
        format!("{{\"day\": \"{day}\", \"runs\": \"3\", \"usd\": \"0.5\"}}"),
    )
    .unwrap();
    assert_eq!(
        build(&fx.cli, &fx.env, fx.dir.path()).used_today(Some(now)),
        Used { runs: 3, usd: 0.5 }
    );
    std::fs::write(
        fx.monid.count_path(),
        format!("{{\"day\": \"{day}\", \"runs\": 2.9, \"usd\": null}}"),
    )
    .unwrap();
    assert_eq!(
        build(&fx.cli, &fx.env, fx.dir.path()).used_today(Some(now)),
        Used { runs: 2, usd: 0.0 }
    );
}

#[test]
fn only_the_last_seven_days_are_kept_in_memory() {
    let fx = fixture();
    let start = 1_790_000_000.0;
    for d in 0..10 {
        assert!(fx
            .monid
            .take(0.0, None, Some(start + 86400.0 * d as f64))
            .is_ok());
    }
    assert_eq!(fx.monid.state.lock().used.len(), 7);
}

#[test]
fn a_run_that_never_started_spends_nothing() {
    let fx = fixture();
    *fx.cli.run_rc.lock() = Some(RC_NO_CLI);
    let opts = RunOpts {
        estimate_usd: SEARCH_USD,
        ..RunOpts::default()
    };
    let out = fx
        .monid
        .run(SEARCH_PROVIDER, SEARCH_ENDPOINT, &json!({}), 5.0, &opts);
    assert_eq!(out["ok"], json!(false));
    assert_eq!(out["reason"], json!("the monid CLI would not start"));
    assert!(out["ms"].is_i64());
    let used = fx.monid.used_today(None);
    assert_eq!((used.runs, used.usd), (1, 0.0));
    assert!(!fx
        .dir
        .path()
        .join("spend")
        .read_dir()
        .unwrap()
        .any(|e| { e.unwrap().path().extension().and_then(|x| x.to_str()) == Some("jsonl") }));
}

#[test]
fn a_nonzero_exit_is_named_and_the_output_is_never_quoted() {
    let fx = fixture();
    *fx.cli.run_reply.lock() = json!("Error: secret-ish text");
    let cli = Arc::new(FakeCli {
        rc: 2,
        ..FakeCli::new()
    });
    *cli.run_reply.lock() = json!("Error: secret-ish text");
    let m = build(&cli, &fx.env, fx.dir.path());
    let out = m.run("p", "/e", &json!({}), 5.0, &RunOpts::default());
    assert_eq!(out["reason"], json!("monid exited 2 without JSON"));
    *cli.run_reply.lock() = json!({"error": "x"});
    let out = m.run("p", "/e", &json!({}), 5.0, &RunOpts::default());
    assert_eq!(out["reason"], json!("monid exited 2"));
    // and the run arguments: the wait is whole seconds, at least 1, and the timeout 3 s past it
    let out = fx
        .monid
        .run("p", "/e", &json!({"a": "é"}), 0.4, &RunOpts::default());
    assert_eq!(out["reason"], json!("monid sent unreadable JSON"));
    let run = fx.cli.runs().pop().unwrap();
    assert_eq!(
        run[1..],
        [
            "run",
            "-p",
            "p",
            "-e",
            "/e",
            "-i",
            "{\"a\": \"\\u00e9\"}",
            "-w",
            "1",
            "-j"
        ]
    );
    assert_eq!(*fx.cli.timeouts.lock().last().unwrap(), 4.0);
}

#[test]
fn the_cli_json_is_read_past_a_notice() {
    assert_eq!(
        cli_json("Update available 0.1.8\n{\"a\": 1}\n"),
        Some(json!({"a": 1}))
    );
    assert_eq!(cli_json("[1, 2]"), Some(json!([1, 2])));
    assert_eq!(cli_json("nothing here"), None);
    assert_eq!(cli_json("{\"a\": 1} trailing"), None);
}

#[test]
fn money_objects_in_every_unit() {
    assert_eq!(usd(&json!(1.5)), Some(1.5));
    assert_eq!(usd(&json!({"value": 25, "unit": "cent"})), Some(0.25));
    assert_eq!(usd(&json!({"value": 3, "currency": "usd"})), Some(3.0));
    assert_eq!(
        usd(&json!({"value": 2, "unit": "MILLI_DOLLAR"})),
        Some(0.002)
    );
    assert_eq!(usd(&json!({"value": 2, "unit": "EUR"})), None);
    assert_eq!(usd(&json!({"value": -1, "unit": "USD"})), None);
    assert_eq!(usd(&json!({"value": true, "unit": "USD"})), None);
    assert_eq!(usd(&json!("2")), None);
    assert_eq!(usd(&Value::Null), None);
    assert_eq!(
        cost(&json!({"cost": {"value": 1, "unit": "USD"}})),
        Some(1.0)
    );
    // a reportedCost that is there but unreadable is no price, even with a cost beside it
    assert_eq!(
        cost(&json!({"billing": {"reportedCost": "x"}, "cost": 1})),
        None
    );
    assert_eq!(
        cost(&json!({"billing": {"reportedCost": null}, "cost": 1})),
        Some(1.0)
    );
    assert_eq!(cost(&json!([1])), None);
}

#[test]
fn the_switches_parse_like_python() {
    let env = |k: &str, v: &str| {
        let mut e = Environ::new();
        e.insert(k.into(), v.into());
        e
    };
    assert_eq!(per_day(Some(&Environ::new())), 20);
    assert_eq!(per_day(Some(&env("EVE_MONID_PER_DAY", " 7.9 "))), 7);
    assert_eq!(per_day(Some(&env("EVE_MONID_PER_DAY", "1e3"))), 1000);
    assert_eq!(per_day(Some(&env("EVE_MONID_PER_DAY", "1_000_000"))), 10000);
    assert_eq!(per_day(Some(&env("EVE_MONID_PER_DAY", "-5"))), 0);
    assert_eq!(per_day(Some(&env("EVE_MONID_PER_DAY", "nan"))), 20);
    assert_eq!(per_day(Some(&env("EVE_MONID_PER_DAY", "ten"))), 20);
    assert_eq!(per_day(Some(&env("EVE_MONID_PER_DAY", "1__0"))), 20);
    assert_eq!(usd_per_day(Some(&Environ::new())), 0.25);
    assert_eq!(usd_per_day(Some(&env("EVE_MONID_USD_PER_DAY", "1.5"))), 1.5);
    assert_eq!(
        usd_per_day(Some(&env("EVE_MONID_USD_PER_DAY", "Infinity"))),
        100.0
    );
    assert_eq!(
        usd_per_day(Some(&env("EVE_MONID_USD_PER_DAY", "-inf"))),
        0.0
    );
    assert_eq!(
        usd_per_day(Some(&env("EVE_MONID_USD_PER_DAY", "NaN"))),
        0.25
    );
    assert_eq!(usd_per_day(Some(&env("EVE_MONID_USD_PER_DAY", "$1"))), 0.25);
}

#[test]
fn search_body_and_parse_search_edges() {
    let long = format!("  a\tb \n{}", "x".repeat(800));
    let body = search_body(&long);
    let kw = body["keyword"].as_str().unwrap();
    assert!(kw.starts_with("a b x") && kw.chars().count() == 700);
    assert_eq!(
        py_dumps(&search_body("q")),
        "{\"keyword\": \"q\", \"location_code\": 2840, \"language_code\": \"en\", \"depth\": 10}"
    );
    // a featured snippet when there is no overview; items at the top level; at most 8 sources
    let mut items =
        vec![json!({"type": "featured_snippet", "title": "T", "description": "  Snip  pet "})];
    for i in 0..10 {
        items.push(json!({"type": "organic", "title": "", "url": format!("https://e/{i}"), "description": format!("d{i}")}));
    }
    let parsed = parse_search(&json!({"items": items}));
    assert_eq!(parsed.sources.len(), MAX_SOURCES);
    assert_eq!(parsed.answer, "From Google results: Snip pet (T) d0");
    // nested tasks[].result[].items[]
    let nested = json!({"tasks": [{"result": [{"items": [{"type": "organic", "title": "A", "url": "u", "description": ""}]}]}]});
    let parsed = parse_search(&nested);
    assert_eq!(
        parsed.sources,
        vec![Source {
            title: "A".into(),
            url: "u".into(),
            snippet: String::new()
        }]
    );
    assert_eq!(parsed.answer, "");
    assert_eq!(parse_search(&json!("text")), Parsed::default());
}

#[test]
fn the_overview_keeps_two_sentences_of_the_first_paragraph() {
    let md = json!("[1]\n**One** is first [2]. Two? Three! Four.\n- list");
    assert_eq!(overview(Some(&md)), "One is first. Two?");
    assert_eq!(
        overview(Some(&json!("Only one sentence"))),
        "Only one sentence"
    );
    assert_eq!(overview(None), "");
    let long = json!(format!("{}.", "w ".repeat(400)));
    assert_eq!(overview(Some(&long)).chars().count(), MAX_SNIPPET_CHARS - 1); // clipped, then stripped
}

#[test]
fn the_web_search_wraps_a_panic() {
    let fx = fixture();
    let boom = |argv: &[String], _t: f64, _e: &HashMap<String, String>| -> (i32, String, String) {
        if argv[1] == "keys" {
            return (0, "[{\"active\": true}]".into(), String::new());
        }
        panic!("runner blew up")
    };
    let opts = SearchOpts {
        runner: Some(Arc::new(boom)),
        ..SearchOpts::default()
    };
    let (out, logs) = capture(|| fx.monid.web_search(QUERY, 25.0, &opts));
    assert_eq!(out, json!({"ok": false, "reason": "the Monid run failed"}));
    assert!(logs.contains("monid: search failed"));
    assert_eq!(
        fx.monid.web_search("   ", 25.0, &SearchOpts::default()),
        json!({"ok": false, "reason": "empty query"})
    );
}

#[test]
fn dirname_matches_os_path() {
    assert_eq!(dirname("/opt/homebrew/bin/monid"), "/opt/homebrew/bin");
    assert_eq!(dirname("monid"), "");
    assert_eq!(dirname("/monid"), "/");
    assert_eq!(dirname("a//b"), "a");
}

#[test]
fn py_dumps_matches_json_dumps() {
    let v = json!({"k": "\u{7f}\u{1F600}\n\"x", "n": [1.0, 1e-05, true, null]});
    assert_eq!(
        py_dumps(&v),
        r#"{"k": "\u007f\ud83d\ude00\n\"x", "n": [1.0, 1e-05, true, null]}"#
    );
}

#[test]
fn the_cli_is_found_on_the_daemons_path() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let fake = bin.join("monid");
    std::fs::write(&fake, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut env = Environ::new();
    env.insert("PATH".into(), format!("/nonexistent:{}", bin.display()));
    env.insert("HOME".into(), tmp.path().to_string_lossy().into_owned());
    let m = Monid::new()
        .with_process_env(env.clone())
        .with_cli_paths(Vec::<String>::new());
    assert_eq!(m.find_cli(None), fake.to_string_lossy());
    // npm's prefix from the environment: <prefix>/bin/monid
    env.insert("PATH".into(), "/nonexistent".into());
    let m = Monid::new()
        .with_process_env(env.clone())
        .with_cli_paths(Vec::<String>::new());
    assert_eq!(m.find_cli(None), "");
    env.insert("NPM_CONFIG_PREFIX".into(), "~".into());
    let m = Monid::new()
        .with_process_env(env)
        .with_cli_paths(Vec::<String>::new());
    assert_eq!(m.find_cli(None), fake.to_string_lossy());
}
