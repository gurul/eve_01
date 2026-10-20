//! Monid: a paid catalogue of data endpoints, used as the web search's last resort. Port of `eve_bridge/monid.py`.
//!
//! Owner, 2026-10-08: "wire monid into eve as a fallback". Monid (monid.ai) sells many data endpoints behind one
//! CLI. eve uses one of them: DataForSEO's live Google results (`dataforseo` `/serp/google-organic`, $0.002 a
//! call of up to 10 results, p50 5.8 s and p95 8.7 s per `monid inspect` on 2026-10-08). websearch.engine_search
//! calls [`web_search`] only when BOTH OpenRouter engines failed, and never after a 4xx other than 429.
//!
//! The CLI holds the key, never eve. eve runs `monid` (the npm package `@monid-ai/cli`) the way firetv runs
//! adb: an argv list (never a shell string), `NO_COLOR=1`, `-j` for JSON, a hard timeout, and a PATH that also has
//! Homebrew's and npm's bin directories (the daemon runs under launchd with a thin PATH, and `monid` is a
//! `#!/usr/bin/env node` script, so node must be on that PATH too). [`available`] reads `monid keys list -j` for
//! an active key and reads only the `active` flag: the key is never read into a log, a result or an error.
//!
//! Spend control, counted HERE and nowhere else (like firecrawl's daily cap): every run takes one from
//! `EVE_MONID_PER_DAY` (default 20 runs a local day) and adds its reported cost to the day's dollars, capped by
//! `EVE_MONID_USD_PER_DAY` (default $0.25). A run that would pass either cap is not sent: one log line, and the
//! caller keeps its own failure. Every run is recorded in eve-spend under provider "monid" with the run's reported
//! cost. `EVE_MONID` = fallback (default) | 0 (off).
//!
//! Nothing here fails: every call returns `{"ok": false, "reason": ...}` instead. Logs carry the shape only (ok,
//! result count, ms, cost), never the query and never the key.
//!
//! Module state (Python's `_USED`, `_LOADED`, `_KEY_OK_AT` and the swappable `RUNNER`) lives in a [`Monid`]; the
//! module-level functions use one process-wide instance. Tests build their own `Monid` with a fake runner, a
//! ledger in a temp folder and an explicit environment, instead of patching globals.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::CString;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use chrono::{Local, TimeZone};
use eve_spend::{Ledger, RecordOpts};
use parking_lot::Mutex;
use regex::Regex;
use serde::Serialize;
use serde_json::{json, Map, Value};

/// launchd's thin PATH misses both.
pub const CLI_PATHS: [&str; 2] = ["/opt/homebrew/bin/monid", "/usr/local/bin/monid"];
/// npm's user-level global prefixes.
pub const NPM_BIN_DIRS: [&str; 2] = ["~/.npm-global/bin", "~/.local/bin"];
/// The runner's code for "no answer in time" (as timeout(1)).
pub const RC_TIMEOUT: i32 = 124;
/// The runner's code for "monid would not start".
pub const RC_NO_CLI: i32 = 127;
/// `monid keys list` (node start-up plus a local file read).
pub const KEYS_SECS: f64 = 8.0;
pub const BALANCE_SECS: f64 = 10.0;
/// A found key is believed for 10 minutes before `keys list` is read again.
pub const KEY_OK_SECS: f64 = 600.0;
pub const DEFAULT_MODE: &str = "fallback";
/// Runs a local day.
pub const DEFAULT_PER_DAY: i64 = 20;
/// Dollars a local day.
pub const DEFAULT_USD_PER_DAY: f64 = 0.25;
/// Beside the spend ledger: `{"day": "YYYY-MM-DD", "runs": n, "usd": x}`.
pub const COUNT_FILE: &str = "monid-runs.json";

// The search endpoint, its input shape as `monid inspect -p dataforseo -e /serp/google-organic` gave it on
// 2026-10-08: body {keyword (required, <= 700 chars), location_code | location_name, language_code | language_name,
// depth (1-200, billed per page of 10), ...}, bodyType json, $0.002 a call (PER_CALL, per page of 10).
pub const SEARCH_PROVIDER: &str = "dataforseo";
pub const SEARCH_ENDPOINT: &str = "/serp/google-organic";
/// The listed price: counted against the day when a run reports no cost.
pub const SEARCH_USD: f64 = 0.002;
/// One page of 10: one $0.002 call.
pub const SEARCH_DEPTH: i64 = 10;
/// United States.
pub const LOCATION_CODE: i64 = 2840;
pub const LANGUAGE_CODE: &str = "en";
/// Monid's share of the web search's budget, at most.
pub const MAX_SEARCH_SECS: f64 = 12.0;
/// Less than this left: not worth paying for a run that cannot finish (p95 8.7 s).
pub const MIN_SEARCH_SECS: f64 = 9.0;
pub const MAX_SOURCES: usize = 8;
pub const MAX_SNIPPET_CHARS: usize = 300;
pub const MAX_ANSWER_CHARS: usize = 1500;

pub const ENV_KEEP: [&str; 8] = [
    "HOME",
    "USER",
    "LOGNAME",
    "LANG",
    "LC_ALL",
    "TMPDIR",
    "SHELL",
    "NPM_CONFIG_PREFIX",
];

/// `UNIT_USD`: a money unit in dollars.
pub const UNIT_USD: [(&str, f64); 5] = [
    ("USD", 1.0),
    ("DOLLAR", 1.0),
    ("CENT", 0.01),
    ("MILLI_DOLLAR", 1e-3),
    ("MICRO_DOLLAR", 1e-6),
];

/// An environment, as Python's `environ` mapping. `None` where one is taken means the process environment.
pub type Environ = HashMap<String, String>;

/// `(argv, timeout, env) -> (rc, stdout, stderr)`. A timeout is an answer ([`RC_TIMEOUT`]), never an error.
pub trait Runner: Send + Sync {
    fn run(
        &self,
        argv: &[String],
        timeout: f64,
        env: &HashMap<String, String>,
    ) -> (i32, String, String);
}

impl<F> Runner for F
where
    F: Fn(&[String], f64, &HashMap<String, String>) -> (i32, String, String) + Send + Sync,
{
    fn run(
        &self,
        argv: &[String],
        timeout: f64,
        env: &HashMap<String, String>,
    ) -> (i32, String, String) {
        self(argv, timeout, env)
    }
}

/// `shutil.which` stand-in: `name` -> its full path, or `None`.
pub type Which = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Today's use: `{"runs": n, "usd": x}`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Default)]
pub struct Used {
    pub runs: i64,
    pub usd: f64,
}

/// One organic result, as websearch's sources carry it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Source {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// [`parse_search`]'s `{"answer", "sources"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct Parsed {
    pub answer: String,
    pub sources: Vec<Source>,
}

/// [`run`]'s keyword arguments (`estimate_usd`, `feature`, `environ`, `runner`, `now`).
#[derive(Clone, Default)]
pub struct RunOpts {
    pub estimate_usd: f64,
    pub feature: String,
    pub environ: Option<Environ>,
    pub runner: Option<Arc<dyn Runner>>,
    pub now: Option<f64>,
}

/// [`web_search`]'s keyword arguments (`environ`, `runner`, `now`).
#[derive(Clone, Default)]
pub struct SearchOpts {
    pub environ: Option<Environ>,
    pub runner: Option<Arc<dyn Runner>>,
    pub now: Option<f64>,
}

#[derive(Default)]
struct State {
    /// Local day -> its use (Python's `_USED`).
    used: BTreeMap<String, Used>,
    /// Days already read back from the count file (Python's `_LOADED`).
    loaded: HashSet<String>,
    /// When a key was last seen active (Python's `_KEY_OK_AT`; `None`: not seen).
    key_ok_at: Option<Instant>,
}

/// The module's state and seams: the runner (`RUNNER`), the spend ledger (whose folder holds [`COUNT_FILE`]), the
/// process environment, `shutil.which`, and the CLI's fallback places.
pub struct Monid {
    runner: Arc<dyn Runner>,
    ledger: Ledger,
    process_env: Option<Environ>,
    which: Option<Which>,
    cli_paths: Vec<String>,
    npm_bin_dirs: Vec<String>,
    state: Mutex<State>,
}

impl Default for Monid {
    fn default() -> Self {
        Self::new()
    }
}

/// Where an `environ` lookup reads: the given mapping, or the process environment.
enum EnvView<'a> {
    Map(&'a Environ),
    Process,
}

impl EnvView<'_> {
    fn get(&self, key: &str) -> Option<String> {
        match self {
            EnvView::Map(m) => m.get(key).cloned(),
            EnvView::Process => std::env::var_os(key).map(|v| v.to_string_lossy().into_owned()),
        }
    }

    /// `str(env.get(key, "") or "").strip()`.
    fn stripped(&self, key: &str) -> String {
        py_strip(&self.get(key).unwrap_or_default()).to_string()
    }
}

static DEFAULT: LazyLock<Monid> = LazyLock::new(Monid::new);

static CITE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[\[\d+\]\]\([^)]*\)|\[\d+\]").expect("cite regex"));
static MARKUP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[*#_`>]+").expect("markup regex"));

impl Monid {
    /// The real thing: [`subprocess_runner`], the default spend ledger, the process environment.
    pub fn new() -> Self {
        Self {
            runner: Arc::new(subprocess_runner),
            ledger: Ledger::default(),
            process_env: None,
            which: None,
            cli_paths: CLI_PATHS.iter().map(|s| s.to_string()).collect(),
            npm_bin_dirs: NPM_BIN_DIRS.iter().map(|s| s.to_string()).collect(),
            state: Mutex::new(State::default()),
        }
    }

    /// Swap the runner (Python's `RUNNER`; tests only).
    pub fn with_runner(mut self, runner: Arc<dyn Runner>) -> Self {
        self.runner = runner;
        self
    }

    /// Record runs in this ledger, and keep [`COUNT_FILE`] in its folder.
    pub fn with_ledger(mut self, ledger: Ledger) -> Self {
        self.ledger = ledger;
        self
    }

    /// Read this mapping wherever Python reads `os.environ` (an `environ` of `None`, the CLI's environment).
    pub fn with_process_env(mut self, env: Environ) -> Self {
        self.process_env = Some(env);
        self
    }

    /// Replace `shutil.which`.
    pub fn with_which(mut self, which: Which) -> Self {
        self.which = Some(which);
        self
    }

    /// Replace [`CLI_PATHS`].
    pub fn with_cli_paths<S: Into<String>>(mut self, paths: impl IntoIterator<Item = S>) -> Self {
        self.cli_paths = paths.into_iter().map(Into::into).collect();
        self
    }

    /// Replace [`NPM_BIN_DIRS`].
    pub fn with_npm_bin_dirs<S: Into<String>>(mut self, dirs: impl IntoIterator<Item = S>) -> Self {
        self.npm_bin_dirs = dirs.into_iter().map(Into::into).collect();
        self
    }

    fn view<'a>(&'a self, environ: Option<&'a Environ>) -> EnvView<'a> {
        match (environ, &self.process_env) {
            (Some(m), _) => EnvView::Map(m),
            (None, Some(m)) => EnvView::Map(m),
            (None, None) => EnvView::Process,
        }
    }

    fn os_env(&self) -> EnvView<'_> {
        self.view(None)
    }

    /// `os.path.expanduser`, reading `HOME` where Python reads `os.environ`.
    fn expanduser(&self, raw: &str) -> String {
        py_expanduser(raw, self.os_env().get("HOME"))
    }

    /// `shutil.which("monid")`: the process PATH (unset: `os.confstr("CS_PATH")`; empty: no match).
    fn which_monid(&self) -> Option<String> {
        if let Some(w) = &self.which {
            return w("monid");
        }
        let path = self.os_env().get("PATH").unwrap_or_else(cs_path);
        if path.is_empty() {
            return None; // PATH='' matches nothing, where PATH=':' looks in the current directory
        }
        // An empty entry joins to a bare "monid", as os.path.join("", "monid") does.
        path.split(':').find_map(|d| {
            let p = Path::new(d).join("monid");
            (p.exists() && !p.is_dir() && access_x(&p)).then(|| p.to_string_lossy().into_owned())
        })
    }

    // ---- running the CLI -----------------------------------------------------------------------------------

    /// `EVE_MONID_CLI`, then the PATH, then Homebrew's two places and npm's user prefixes. "" when none.
    pub fn find_cli(&self, environ: Option<&Environ>) -> String {
        let env = self.view(environ);
        let over = env.stripped("EVE_MONID_CLI");
        if !over.is_empty() {
            return over;
        }
        if let Some(found) = self.which_monid().filter(|f| !f.is_empty()) {
            return found;
        }
        let mut places: Vec<String> = self.cli_paths.clone();
        places.extend(self.npm_bin_dirs.iter().map(|d| {
            Path::new(&self.expanduser(d))
                .join("monid")
                .to_string_lossy()
                .into_owned()
        }));
        let prefix = env.stripped("NPM_CONFIG_PREFIX");
        if !prefix.is_empty() {
            places.push(
                Path::new(&self.expanduser(&prefix))
                    .join("bin")
                    .join("monid")
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        places
            .into_iter()
            .find(|p| access_x(Path::new(p)))
            .unwrap_or_default()
    }

    /// The CLI's environment: only what node and the CLI need (its own key lives in its own config), never eve's
    /// secrets (review, 2026-10-08: the whole environment, OpenRouter and Telegram keys included, reached a
    /// third-party npm package). NO_COLOR=1, and a PATH where `node` is found (the CLI's shebang).
    pub fn cli_env(&self, cli: &str) -> HashMap<String, String> {
        let os = self.os_env();
        let mut env: HashMap<String, String> = HashMap::new();
        for k in ENV_KEEP {
            if let Some(v) = os.get(k).filter(|v| !v.is_empty()) {
                env.insert(k.to_string(), v);
            }
        }
        env.insert("NO_COLOR".into(), "1".into());
        let mut dirs: Vec<String> = Vec::new();
        let head = dirname(cli);
        if !head.is_empty() {
            dirs.push(head);
        }
        dirs.push("/opt/homebrew/bin".into());
        dirs.push("/usr/local/bin".into());
        // The daemon's own PATH (nvm's node lives there), not env's: env was just built without one. /usr/bin and
        // /bin always, for the shebang's /usr/bin/env.
        let path = os.get("PATH").unwrap_or_default();
        dirs.extend(path.split(':').filter(|d| !d.is_empty()).map(String::from));
        dirs.push("/usr/bin".into());
        dirs.push("/bin".into());
        let mut seen = HashSet::new();
        let ordered: Vec<String> = dirs
            .into_iter()
            .filter(|d| seen.insert(d.clone()))
            .collect();
        env.insert("PATH".into(), ordered.join(":"));
        env
    }

    /// `(Some(parsed JSON), "", true)` or `(None, reason, started)`: `started` is false when no process ran
    /// (nothing can have been spent). The reason never quotes the CLI's output (it could hold anything).
    fn call(
        &self,
        args: &[&str],
        timeout: f64,
        environ: Option<&Environ>,
        runner: Option<&dyn Runner>,
    ) -> (Option<Value>, String, bool) {
        let cli = self.find_cli(environ);
        if cli.is_empty() {
            return (None, "the monid CLI is not installed".into(), false);
        }
        let mut argv: Vec<String> = vec![cli.clone()];
        argv.extend(args.iter().map(|a| a.to_string()));
        argv.push("-j".into());
        let env = self.cli_env(&cli);
        let (rc, out, _err) = match runner {
            Some(r) => r.run(&argv, timeout, &env),
            None => self.runner.run(&argv, timeout, &env),
        };
        if rc == RC_TIMEOUT {
            return (
                None,
                format!("monid did not answer in {timeout:.0} s"),
                true,
            );
        }
        if rc == RC_NO_CLI {
            return (None, "the monid CLI would not start".into(), false);
        }
        let Some(data) = cli_json(&out) else {
            let reason = if rc != 0 {
                format!("monid exited {rc} without JSON")
            } else {
                "monid sent unreadable JSON".into()
            };
            return (None, reason, true);
        };
        if rc != 0 {
            return (None, format!("monid exited {rc}"), true);
        }
        (Some(data), String::new(), true)
    }

    // ---- switches and the daily cap ----------------------------------------------------------------------------

    /// EVE_MONID: "fallback" (default) or "off" (0, off, false, no).
    pub fn mode(&self, environ: Option<&Environ>) -> &'static str {
        mode_in(&self.view(environ))
    }

    /// EVE_MONID_PER_DAY: Monid runs a local day (default 20; 0 stops them all).
    pub fn per_day(&self, environ: Option<&Environ>) -> i64 {
        per_day_in(&self.view(environ))
    }

    /// EVE_MONID_USD_PER_DAY: dollars of Monid runs a local day (default 0.25; 0 stops them all).
    pub fn usd_per_day(&self, environ: Option<&Environ>) -> f64 {
        usd_per_day_in(&self.view(environ))
    }

    /// The count file: [`COUNT_FILE`] in the spend ledger's folder.
    pub fn count_path(&self) -> PathBuf {
        self.ledger.dir().join(COUNT_FILE)
    }

    /// Once a process per day: the day's use saved by an earlier process (a restart must not hand out a fresh
    /// cap). Under the lock. An unreadable file counts as nothing saved.
    fn load(&self, st: &mut State, day: &str) {
        if st.loaded.contains(day) {
            return;
        }
        st.loaded.insert(day.to_string());
        let Ok(bytes) = std::fs::read(self.count_path()) else {
            return;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            return;
        };
        let Ok(saved) = serde_json::from_str::<Value>(&text) else {
            return;
        };
        let Some(saved) = saved.as_object() else {
            return;
        };
        if saved.get("day").and_then(Value::as_str) != Some(day) {
            return;
        }
        let used = st.used.entry(day.to_string()).or_default();
        let Some(runs) = py_int_value(saved.get("runs")) else {
            return;
        };
        used.runs = used.runs.max(runs);
        let Some(usd) = py_float_value(saved.get("usd")) else {
            return;
        };
        // Python's max(used, saved): the saved figure only when it is larger (a NaN never wins).
        if usd > used.usd {
            used.usd = usd;
        }
    }

    /// Under the lock. Best effort: a failed write only loses the count across a restart.
    fn save(&self, st: &State, day: &str) {
        let path = self.count_path();
        let used = st.used.get(day).copied().unwrap_or_default();
        let text = format!(
            "{{\"day\": {}, \"runs\": {}, \"usd\": {}}}",
            py_dumps(&Value::String(day.to_string())),
            used.runs,
            eve_spend::py_float_repr(py_round(used.usd, 6))
        );
        let write = || -> std::io::Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, text.as_bytes())?;
            std::fs::rename(&tmp, &path)
        };
        if let Err(e) = write() {
            tracing::warn!(
                "monid: could not save today's run count ({})",
                os_error_name(&e)
            );
        }
    }

    /// `{"runs": n, "usd": x}` spent today (`now`: an epoch time, default now).
    pub fn used_today(&self, now: Option<f64>) -> Used {
        let Some(day) = local_day(now) else {
            return Used::default();
        };
        let mut st = self.state.lock();
        self.load(&mut st, &day);
        st.used.get(&day).copied().unwrap_or_default()
    }

    /// Count one run against today's caps before it is sent: `Ok(())` when it may go, else which cap is spent.
    /// `Err(None)` when `now` is not a time Python's `datetime` can hold (Python raises there).
    fn take(
        &self,
        estimate: f64,
        environ: Option<&Environ>,
        now: Option<f64>,
    ) -> Result<(), Option<String>> {
        let day = local_day(now).ok_or(None)?;
        let mut st = self.state.lock();
        self.load(&mut st, &day);
        let used = st.used.entry(day.clone()).or_default();
        let per_day = self.per_day(environ);
        if used.runs >= per_day {
            return Err(Some(format!(
                "today's {per_day} Monid runs are used up (EVE_MONID_PER_DAY)"
            )));
        }
        let usd_cap = self.usd_per_day(environ);
        if used.usd + estimate > usd_cap + 1e-9 {
            return Err(Some(format!(
                "today's ${usd_cap:.2} of Monid runs is used up (EVE_MONID_USD_PER_DAY)"
            )));
        }
        used.runs += 1;
        used.usd += estimate;
        let n = st.used.len();
        if n > 7 {
            let old: Vec<String> = st.used.keys().take(n - 7).cloned().collect();
            for k in old {
                st.used.remove(&k);
            }
        }
        self.save(&st, &day);
        Ok(())
    }

    /// Swap a run's estimate for its reported cost in today's dollars (a run with no reported cost keeps it).
    fn settle(&self, estimate: f64, cost: Option<f64>, now: Option<f64>) {
        let Some(cost) = cost else { return };
        let Some(day) = local_day(now) else { return };
        let mut st = self.state.lock();
        self.load(&mut st, &day);
        let used = st.used.entry(day.clone()).or_default();
        let x = used.usd - estimate + cost;
        // Python's max(0.0, x): 0.0 unless x is larger.
        used.usd = if x > 0.0 { x } else { 0.0 };
        self.save(&st, &day);
    }

    /// Forget today's counts and the key check (tests).
    pub fn reset_for_tests(&self) {
        let mut st = self.state.lock();
        st.used.clear();
        st.loaded.clear();
        st.key_ok_at = None;
    }

    // ---- the three calls ---------------------------------------------------------------------------------------

    /// The CLI is found and holds an active key (`monid keys list -j`; only the `active` flag is read). The
    /// check waits at most `timeout` (a search passes what is left of its budget; Python's default is
    /// [`KEYS_SECS`]).
    pub fn available(
        &self,
        environ: Option<&Environ>,
        runner: Option<&dyn Runner>,
        timeout: f64,
    ) -> bool {
        let fresh = self
            .state
            .lock()
            .key_ok_at
            .is_some_and(|t| t.elapsed().as_secs_f64() < KEY_OK_SECS);
        if fresh && !self.find_cli(environ).is_empty() {
            return true;
        }
        let (data, reason, _) = self.call(
            &["keys", "list"],
            py_max(1.0, py_min(KEYS_SECS, timeout)),
            environ,
            runner,
        );
        let Some(data) = data else {
            tracing::info!("monid: not available ({reason})");
            return false;
        };
        let items = match &data {
            Value::Object(m) => m.get("items"),
            other => Some(other),
        };
        let ok = items.and_then(Value::as_array).is_some_and(|items| {
            items.iter().any(|k| {
                k.as_object()
                    .is_some_and(|k| k.get("active") == Some(&Value::Bool(true)))
            })
        });
        if ok {
            self.state.lock().key_ok_at = Some(Instant::now());
        } else {
            tracing::info!("monid: not available (no active key; `monid keys add` sets one)");
        }
        ok
    }

    /// One Monid run, waited for up to `wait_secs` (the process is killed a little after). Never fails.
    /// `{"ok": true, "output", "cost_usd", "status", "ms"}` or `{"ok": false, "reason"}` (`capped` when a daily cap
    /// refused it). Takes a run from today's caps before it is sent and records it in eve-spend after.
    pub fn run(
        &self,
        provider: &str,
        endpoint: &str,
        body: &Value,
        wait_secs: f64,
        opts: &RunOpts,
    ) -> Value {
        let environ = opts.environ.as_ref();
        if self.mode(environ) == "off" {
            return json!({"ok": false, "reason": "Monid is switched off (EVE_MONID=0)"});
        }
        if self.find_cli(environ).is_empty() {
            return json!({"ok": false, "reason": "the monid CLI is not installed"});
        }
        if !wait_secs.is_finite() {
            // Python's int(wait_secs) raises here; web_search turns that into this same answer.
            tracing::warn!("monid: run failed (wait_secs is not a number)");
            return json!({"ok": false, "reason": "the Monid run failed"});
        }
        let wait = (wait_secs.trunc() as i64).max(1);
        match self.take(opts.estimate_usd, environ, opts.now) {
            Ok(()) => {}
            Err(Some(refused)) => {
                tracing::info!("monid: {refused}; not sent");
                return json!({"ok": false, "reason": refused, "capped": true});
            }
            Err(None) => {
                tracing::warn!("monid: run failed (the time is out of range)");
                return json!({"ok": false, "reason": "the Monid run failed"});
            }
        }
        let t0 = Instant::now();
        let input = py_dumps(body);
        let wait_s = wait.to_string();
        let (data, reason, started) = self.call(
            &[
                "run", "-p", provider, "-e", endpoint, "-i", &input, "-w", &wait_s,
            ],
            wait as f64 + 3.0,
            environ,
            opts.runner.as_deref(),
        );
        let ms = elapsed_ms(t0);
        let cost = data.as_ref().and_then(cost_of);
        self.settle(
            opts.estimate_usd,
            if started { cost } else { Some(0.0) },
            opts.now,
        ); // nothing ran: nothing spent
        let what = format!("{provider}{endpoint}");
        let status = match &data {
            Some(Value::Object(m)) => py_str(m.get("status")),
            _ => String::new(),
        };
        if started {
            let feature = if opts.feature.is_empty() {
                eve_spend::SEARCH
            } else {
                opts.feature.as_str()
            };
            let note = if cost.is_some() {
                String::new()
            } else {
                format!(
                    "no reported cost ({}); ~${} counted against the day",
                    if status.is_empty() {
                        "no reply"
                    } else {
                        &status
                    },
                    eve_spend::py_float_repr(opts.estimate_usd)
                )
            };
            let source = if cost.is_some() { "reported" } else { "priced" };
            self.ledger.record(
                "monid",
                &what,
                feature,
                cost,
                RecordOpts::new().source(source).note(&note),
            );
        }
        let Some(data) = data else {
            tracing::warn!("monid: run failed in {ms} ms ({reason})");
            return json!({"ok": false, "reason": reason, "ms": ms});
        };
        if status != "COMPLETED" {
            let ended = if status.is_empty() {
                "without a status"
            } else {
                status.as_str()
            };
            tracing::warn!("monid: run ended {ended} in {ms} ms");
            return json!({"ok": false, "reason": format!("the Monid run ended {ended}"), "ms": ms, "status": status});
        }
        let output = data.get("output").cloned().unwrap_or(Value::Null);
        json!({"ok": true, "output": output, "cost_usd": cost, "status": status, "ms": ms})
    }

    /// `{"ok": true, "usd": x, "held_usd": y}` from `monid balance -j`, or `{"ok": false, "reason"}`.
    pub fn balance(&self, environ: Option<&Environ>, runner: Option<&dyn Runner>) -> Value {
        let (data, reason, _) = self.call(&["balance"], BALANCE_SECS, environ, runner);
        let Some(data) = data else {
            return json!({"ok": false, "reason": reason});
        };
        let obj = data.as_object();
        let usd = obj.and_then(|m| usd_of(m.get("balance").unwrap_or(&Value::Null)));
        let held = obj.and_then(|m| usd_of(m.get("held").unwrap_or(&Value::Null)));
        let Some(usd) = usd else {
            return json!({"ok": false, "reason": "monid sent no balance"});
        };
        // `held or 0.0`: a held of 0 is 0.0 too.
        json!({"ok": true, "usd": usd, "held_usd": held.filter(|h| *h != 0.0).unwrap_or(0.0)})
    }

    /// [`Monid::run`] off the async runtime (on tokio's blocking pool, carrying the spend job tag).
    pub async fn arun(
        self: Arc<Self>,
        provider: String,
        endpoint: String,
        body: Value,
        wait_secs: f64,
        opts: RunOpts,
    ) -> Value {
        eve_spend::to_thread(move || self.run(&provider, &endpoint, &body, wait_secs, &opts)).await
    }

    /// [`Monid::balance`] off the async runtime.
    pub async fn abalance(
        self: Arc<Self>,
        environ: Option<Environ>,
        runner: Option<Arc<dyn Runner>>,
    ) -> Value {
        eve_spend::to_thread(move || self.balance(environ.as_ref(), runner.as_deref())).await
    }

    // ---- the web search fallback ---------------------------------------------------------------------------------

    /// The web search's last resort. Never fails (review, 2026-10-08: odd output or settings escaped as
    /// OverflowError/RecursionError): a panic inside comes back as `{"ok": false, "reason": "the Monid run failed"}`.
    pub fn web_search(&self, query: &str, budget_secs: f64, opts: &SearchOpts) -> Value {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.web_search_inner(query, budget_secs, opts)
        })) {
            Ok(v) => v,
            Err(_) => {
                // a last resort that fails returns the search's own failure
                tracing::warn!("monid: search failed (panic)");
                json!({"ok": false, "reason": "the Monid run failed"})
            }
        }
    }

    /// Google results through Monid, in websearch's result shape (`{"ok", "answer", "sources", "usage", "ms",
    /// "cost_usd", "fallback": "monid"}`). Waits at most min(budget_secs, MAX_SEARCH_SECS); `{"ok": false,
    /// "reason"}` when it could not answer.
    fn web_search_inner(&self, query: &str, budget_secs: f64, opts: &SearchOpts) -> Value {
        let environ = opts.environ.as_ref();
        if self.mode(environ) == "off" {
            return json!({"ok": false, "reason": "Monid is switched off (EVE_MONID=0)"});
        }
        if py_strip(query).is_empty() {
            return json!({"ok": false, "reason": "empty query"});
        }
        let t0 = Instant::now();
        let budget = py_min(MAX_SEARCH_SECS, budget_secs);
        if budget < MIN_SEARCH_SECS {
            tracing::info!(
                "monid: {:.1} s left of the search; not worth a run",
                py_max(0.0, budget)
            );
            return json!({"ok": false, "reason": "no time left for Monid"});
        }
        if !self.available(
            environ,
            opts.runner.as_deref(),
            budget - MIN_SEARCH_SECS + 1.0,
        ) {
            return json!({"ok": false, "reason": "Monid is not set up on this computer"});
        }
        let left = budget - t0.elapsed().as_secs_f64();
        if left < MIN_SEARCH_SECS {
            return json!({"ok": false, "reason": "no time left for Monid"});
        }
        let run_opts = RunOpts {
            estimate_usd: SEARCH_USD,
            feature: eve_spend::SEARCH.to_string(),
            environ: opts.environ.clone(),
            runner: opts.runner.clone(),
            now: opts.now,
        };
        let out = self.run(
            SEARCH_PROVIDER,
            SEARCH_ENDPOINT,
            &search_body(query),
            if left > 4.0 { left - 3.0 } else { 1.0 },
            &run_opts,
        );
        let ms = elapsed_ms(t0);
        if out.get("ok") != Some(&Value::Bool(true)) {
            let reason = out
                .get("reason")
                .and_then(Value::as_str)
                .filter(|r| !r.is_empty());
            return json!({"ok": false, "reason": reason.unwrap_or("the Monid run failed")});
        }
        let parsed = parse_search(out.get("output").unwrap_or(&Value::Null));
        let cost = out.get("cost_usd").and_then(Value::as_f64);
        if parsed.sources.is_empty() && parsed.answer.is_empty() {
            let shown = cost
                .map(eve_spend::py_float_repr)
                .unwrap_or_else(|| "None".into());
            tracing::warn!("monid: search ok in {ms} ms but no results, ${shown}");
            return json!({"ok": false, "reason": "Monid found no results"});
        }
        let mut usage = Map::new();
        usage.insert("in".into(), json!(0));
        usage.insert("out".into(), json!(0));
        if let Some(c) = cost {
            usage.insert("cost".into(), json!(c));
        }
        let shown = cost
            .map(|c| format!("{c:.4}"))
            .unwrap_or_else(|| "?".into());
        tracing::info!(
            "monid: search ok, {} results in {ms} ms, ${shown}",
            parsed.sources.len()
        );
        json!({
            "ok": true,
            "answer": parsed.answer,
            "sources": parsed.sources,
            "usage": usage,
            "ms": ms,
            "cost_usd": cost.map(|c| py_round(c, 4)),
            "fallback": "monid",
        })
    }
}

// ---- the module-level functions (one process-wide Monid) --------------------------------------------------------

/// `EVE_MONID_CLI`, then the PATH, then Homebrew's two places and npm's user prefixes. "" when none.
pub fn find_cli(environ: Option<&Environ>) -> String {
    DEFAULT.find_cli(environ)
}

/// Python's `_env`: the CLI's minimal environment (see [`Monid::cli_env`]).
pub fn cli_env(cli: &str) -> HashMap<String, String> {
    DEFAULT.cli_env(cli)
}

/// The real runner: one CLI process, an argv list (never a shell string), a hard timeout (the child is killed).
pub fn subprocess_runner(
    argv: &[String],
    timeout: f64,
    env: &HashMap<String, String>,
) -> (i32, String, String) {
    let Some((prog, args)) = argv.split_first() else {
        return (
            RC_NO_CLI,
            String::new(),
            "monid could not start (IndexError)".into(),
        );
    };
    let spawned = Command::new(prog)
        .args(args)
        .env_clear()
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            return (
                RC_NO_CLI,
                String::new(),
                format!("monid could not start ({})", strerror(&e)),
            )
        }
    };
    // A timeout too large for the clock is no deadline at all.
    let deadline = Duration::try_from_secs_f64(timeout.max(0.0))
        .ok()
        .and_then(|d| Instant::now().checked_add(d));
    let (tx, rx) = mpsc::channel::<(bool, Vec<u8>)>();
    if let Some(mut out) = child.stdout.take() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = out.read_to_end(&mut buf);
            let _ = tx.send((true, buf));
        });
    }
    if let Some(mut err) = child.stderr.take() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = err.read_to_end(&mut buf);
            let _ = tx.send((false, buf));
        });
    }
    drop(tx);
    // Like subprocess.run: both pipes read to their end and the process gone, all within the timeout.
    let (mut stdout, mut stderr) = (None::<Vec<u8>>, None::<Vec<u8>>);
    let mut status = None;
    loop {
        if status.is_none() {
            match child.try_wait() {
                Ok(Some(s)) => status = Some(s),
                Ok(None) => {}
                Err(_) => break,
            }
        }
        while let Ok((is_out, buf)) = rx.try_recv() {
            if is_out {
                stdout = Some(buf);
            } else {
                stderr = Some(buf);
            }
        }
        if let (Some(s), Some(_), Some(_)) = (status, &stdout, &stderr) {
            return (
                exit_code(s),
                lossy(stdout.as_deref()),
                lossy(stderr.as_deref()),
            );
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = child.kill();
    let _ = child.wait();
    (RC_TIMEOUT, String::new(), String::new())
}

/// EVE_MONID: "fallback" (default) or "off" (0, off, false, no).
pub fn mode(environ: Option<&Environ>) -> &'static str {
    DEFAULT.mode(environ)
}

/// EVE_MONID_PER_DAY: Monid runs a local day (default 20; 0 stops them all).
pub fn per_day(environ: Option<&Environ>) -> i64 {
    DEFAULT.per_day(environ)
}

/// EVE_MONID_USD_PER_DAY: dollars of Monid runs a local day (default 0.25; 0 stops them all).
pub fn usd_per_day(environ: Option<&Environ>) -> f64 {
    DEFAULT.usd_per_day(environ)
}

/// `{"runs": n, "usd": x}` spent today.
pub fn used_today(now: Option<f64>) -> Used {
    DEFAULT.used_today(now)
}

/// Forget the process-wide counts and key check (tests).
pub fn reset_for_tests() {
    DEFAULT.reset_for_tests()
}

/// See [`Monid::available`]. Python's default `timeout` is [`KEYS_SECS`].
pub fn available(environ: Option<&Environ>, runner: Option<&dyn Runner>, timeout: f64) -> bool {
    DEFAULT.available(environ, runner, timeout)
}

/// See [`Monid::run`].
pub fn run(provider: &str, endpoint: &str, body: &Value, wait_secs: f64, opts: &RunOpts) -> Value {
    DEFAULT.run(provider, endpoint, body, wait_secs, opts)
}

/// See [`Monid::balance`].
pub fn balance(environ: Option<&Environ>, runner: Option<&dyn Runner>) -> Value {
    DEFAULT.balance(environ, runner)
}

/// `run` off the async runtime.
pub async fn arun(
    provider: String,
    endpoint: String,
    body: Value,
    wait_secs: f64,
    opts: RunOpts,
) -> Value {
    eve_spend::to_thread(move || DEFAULT.run(&provider, &endpoint, &body, wait_secs, &opts)).await
}

/// `balance` off the async runtime.
pub async fn abalance(environ: Option<Environ>, runner: Option<Arc<dyn Runner>>) -> Value {
    eve_spend::to_thread(move || DEFAULT.balance(environ.as_ref(), runner.as_deref())).await
}

/// See [`Monid::web_search`].
pub fn web_search(query: &str, budget_secs: f64, opts: &SearchOpts) -> Value {
    DEFAULT.web_search(query, budget_secs, opts)
}

/// One query, one page of 10, United States, English: one $0.002 call.
pub fn search_body(query: &str) -> Value {
    let keyword: String = py_words(query).chars().take(700).collect();
    json!({"keyword": keyword, "location_code": LOCATION_CODE, "language_code": LANGUAGE_CODE, "depth": SEARCH_DEPTH})
}

/// `{"answer", "sources"}` from a Google results run: the organic results as sources, and a short answer built
/// from what Google showed on top (its AI overview, else a featured snippet) and the top organic snippets, two
/// lines in all. No model call.
pub fn parse_search(output: &Value) -> Parsed {
    let items = items_of(output);
    let mut sources: Vec<Source> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for it in &items {
        if it.get("type") != Some(&Value::String("organic".into())) {
            continue;
        }
        let url = clean(it.get("url"), 500);
        if url.is_empty() || seen.contains(&url) {
            continue;
        }
        seen.insert(url.clone());
        sources.push(Source {
            title: clean(it.get("title"), 120),
            url,
            snippet: clean(it.get("description"), MAX_SNIPPET_CHARS),
        });
        if sources.len() >= MAX_SOURCES {
            break;
        }
    }
    let mut lines: Vec<String> = Vec::new();
    for it in &items {
        let kind = it.get("type").and_then(Value::as_str);
        if kind == Some("ai_overview") {
            let ov = overview(it.get("markdown"));
            if !ov.is_empty() {
                lines.push(format!("{ov} (Google AI overview)"));
                break;
            }
        }
        if kind == Some("featured_snippet") {
            let desc = clean(it.get("description"), MAX_SNIPPET_CHARS);
            if !desc.is_empty() {
                lines.push(format!("{desc} ({})", clean(it.get("title"), 120)));
                break;
            }
        }
    }
    for s in &sources {
        if lines.len() >= 2 {
            break;
        }
        if !s.snippet.is_empty() {
            lines.push(if s.title.is_empty() {
                s.snippet.clone()
            } else {
                format!("{} ({})", s.snippet, s.title)
            });
        }
    }
    let answer = if lines.is_empty() {
        String::new()
    } else {
        format!("From Google results: {}", lines.join(" "))
            .chars()
            .take(MAX_ANSWER_CHARS)
            .collect()
    };
    Parsed { answer, sources }
}

// ---- the pieces ------------------------------------------------------------------------------------------------

fn mode_in(env: &EnvView<'_>) -> &'static str {
    let raw = env.stripped("EVE_MONID").to_lowercase();
    if matches!(raw.as_str(), "0" | "off" | "false" | "no") {
        "off"
    } else {
        DEFAULT_MODE
    }
}

fn per_day_in(env: &EnvView<'_>) -> i64 {
    let raw = env.stripped("EVE_MONID_PER_DAY");
    if raw.is_empty() {
        return DEFAULT_PER_DAY;
    }
    match py_float(&raw).filter(|f| f.is_finite()) {
        // int(float(raw)), clamped to 0..=10000
        Some(f) => f.trunc().clamp(0.0, 10000.0) as i64,
        None => {
            tracing::warn!("monid: EVE_MONID_PER_DAY is not a number; {DEFAULT_PER_DAY} a day");
            DEFAULT_PER_DAY
        }
    }
}

fn usd_per_day_in(env: &EnvView<'_>) -> f64 {
    let raw = env.stripped("EVE_MONID_USD_PER_DAY");
    let usd = if raw.is_empty() {
        DEFAULT_USD_PER_DAY
    } else {
        match py_float(&raw) {
            Some(f) => f,
            None => {
                tracing::warn!(
                    "monid: EVE_MONID_USD_PER_DAY is not a number; ${DEFAULT_USD_PER_DAY:.2} a day"
                );
                return DEFAULT_USD_PER_DAY;
            }
        }
    };
    if usd.is_nan() {
        DEFAULT_USD_PER_DAY
    } else {
        usd.clamp(0.0, 100.0)
    }
}

/// The local day of `now` (default now), `YYYY-MM-DD`. `None` when `datetime.fromtimestamp` would raise.
fn local_day(now: Option<f64>) -> Option<String> {
    let dt = match now {
        None => Local::now(),
        Some(ts) => {
            if !ts.is_finite() {
                return None;
            }
            let secs = ts.floor();
            if !(-62_135_596_800.0..=253_402_300_799.0).contains(&secs) {
                return None;
            }
            let nanos = ((ts - secs) * 1e9).round().clamp(0.0, 999_999_999.0) as u32;
            Local.timestamp_opt(secs as i64, nanos).earliest()?
        }
    };
    Some(dt.format("%Y-%m-%d").to_string())
}

/// The CLI's JSON, past any line it printed before it (an update notice). `None` when there is none to read.
fn cli_json(text: &str) -> Option<Value> {
    let text = py_strip(text);
    let start = [text.find('{'), text.find('[')]
        .into_iter()
        .flatten()
        .min()?;
    serde_json::from_str(&text[start..]).ok()
}

/// A money object (`{"value", "unit"?}`) or a bare number, in dollars; `None` when it is not one.
fn usd_of(value: &Value) -> Option<f64> {
    let mut unit = 1.0;
    let mut value = value;
    if let Value::Object(m) = value {
        // no unit is no price: the CLI reports micro-dollars, and reading a bare 2000 as dollars would record a
        // $0.002 run as $2000 (review, 2026-10-08)
        let name = {
            let u = py_str(m.get("unit"));
            if u.is_empty() {
                py_str(m.get("currency"))
            } else {
                u
            }
        }
        .to_uppercase();
        unit = UNIT_USD
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| *v)
            .unwrap_or(-1.0);
        value = m.get("value").unwrap_or(&Value::Null);
    }
    let Value::Number(n) = value else { return None };
    let x = n.as_f64()?;
    if unit < 0.0 || x.is_nan() || x < 0.0 {
        return None;
    }
    Some(x * unit)
}

/// A run's reported cost in dollars. The 0.1.7 CLI reports `billing.reportedCost` as `{"value": 2000, "unit":
/// "MICRO_DOLLAR"}` (a $0.002 run, 2026-10-08); the skill documents `cost.value`. Neither: `None`.
fn cost_of(data: &Value) -> Option<f64> {
    let m = data.as_object()?;
    if let Some(Value::Object(billing)) = m.get("billing") {
        if let Some(rc) = billing.get("reportedCost").filter(|v| !v.is_null()) {
            return usd_of(rc);
        }
    }
    usd_of(m.get("cost").unwrap_or(&Value::Null))
}

/// Python's `_cost`, public for callers and tests.
pub fn cost(data: &Value) -> Option<f64> {
    cost_of(data)
}

/// Python's `_usd`, public for callers and tests.
pub fn usd(value: &Value) -> Option<f64> {
    usd_of(value)
}

/// The SERP items, wherever the output nests them (DataForSEO's tasks[].result[].items[], or items at the top).
fn items_of(output: &Value) -> Vec<&Map<String, Value>> {
    match output {
        Value::Array(parts) => parts.iter().flat_map(items_of).collect(),
        Value::Object(m) => {
            if let Some(Value::Array(items)) = m.get("items") {
                if items
                    .iter()
                    .any(|i| i.as_object().is_some_and(|i| i.contains_key("type")))
                {
                    return items.iter().filter_map(Value::as_object).collect();
                }
            }
            let mut found = Vec::new();
            for key in ["tasks", "result", "results", "data", "output"] {
                if let Some(v) = m.get(key) {
                    found.extend(items_of(v));
                }
            }
            found
        }
        _ => Vec::new(),
    }
}

/// `" ".join(str(text or "").split())[:limit]`.
fn clean(text: Option<&Value>, limit: usize) -> String {
    py_words(&py_str(text)).chars().take(limit).collect()
}

/// The first two sentences of the first paragraph of Google's AI overview (a list under it is left out), its
/// citation links and markdown taken out.
fn overview(markdown: Option<&Value>) -> String {
    let md = py_str(markdown);
    let first = md
        .split('\n')
        .find(|p| {
            !py_words(&CITE.replace_all(p, ""))
                .chars()
                .take(10)
                .collect::<String>()
                .is_empty()
        })
        .unwrap_or("");
    let no_cite = CITE.replace_all(first, "");
    let text: String = py_words(&MARKUP.replace_all(&no_cite, ""))
        .chars()
        .take(2000)
        .collect();
    let text = text.replace(" ,", ",").replace(" .", ".");
    // re.split(r"(?<=[.!?])\s+", text)[:2], joined by " ": the text is single-spaced, so this is everything before
    // the second space that follows a sentence end.
    let mut boundaries = 0;
    let mut prev: Option<char> = None;
    let mut cut = text.len();
    for (i, c) in text.char_indices() {
        if py_isspace(c) && matches!(prev, Some('.' | '!' | '?')) {
            boundaries += 1;
            if boundaries == 2 {
                cut = i;
                break;
            }
        }
        prev = Some(c);
    }
    let two: String = text[..cut].chars().take(MAX_SNIPPET_CHARS).collect();
    py_strip(&two).to_string()
}

/// Python's `str.isspace` for one character.
fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

fn py_strip(s: &str) -> &str {
    s.trim_matches(py_isspace)
}

/// `" ".join(s.split())`.
fn py_words(s: &str) -> String {
    s.split(py_isspace)
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// `str(v or "")`: a falsy value is "", a string itself, a number its repr. Lists and objects print as JSON
/// (Python prints their repr).
fn py_str(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) | Some(Value::Bool(false)) => String::new(),
        Some(Value::Bool(true)) => "True".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => {
            if let Some(i) = n.as_i64() {
                if i == 0 {
                    String::new()
                } else {
                    i.to_string()
                }
            } else if let Some(u) = n.as_u64() {
                u.to_string()
            } else {
                let f = n.as_f64().unwrap_or(0.0);
                if f == 0.0 {
                    String::new()
                } else {
                    eve_spend::py_float_repr(f)
                }
            }
        }
        Some(Value::Array(a)) if a.is_empty() => String::new(),
        Some(Value::Object(o)) if o.is_empty() => String::new(),
        Some(other) => other.to_string(),
    }
}

/// Python's `float(str)`: surrounding whitespace, underscores between digits, inf and nan in any case.
fn py_float(raw: &str) -> Option<f64> {
    let t = py_strip(raw);
    if t.is_empty() {
        return None;
    }
    let t = strip_underscores(t)?;
    let lower = t.to_ascii_lowercase();
    let body = lower.trim_start_matches(['+', '-']);
    if body.len() + 1 < lower.len() {
        return None; // more than one sign
    }
    if matches!(body, "inf" | "infinity" | "nan")
        || body
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | '+' | '-'))
    {
        lower.parse::<f64>().ok()
    } else {
        None
    }
}

/// Python's `int(str)` for the plain decimal forms.
fn py_int_str(raw: &str) -> Option<i64> {
    let t = strip_underscores(py_strip(raw))?;
    let digits = t.strip_prefix(['+', '-']).unwrap_or(&t);
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    t.parse::<i64>().ok()
}

/// Underscores are allowed only between two digits; they are dropped.
fn strip_underscores(t: &str) -> Option<String> {
    if !t.contains('_') {
        return Some(t.to_string());
    }
    let chars: Vec<char> = t.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if *c == '_' {
            let before = i > 0 && chars[i - 1].is_ascii_digit();
            let after = chars.get(i + 1).is_some_and(|c| c.is_ascii_digit());
            if !(before && after) {
                return None;
            }
        }
    }
    Some(chars.into_iter().filter(|c| *c != '_').collect())
}

/// `int(v or 0)` on a JSON value; `None` where Python raises.
fn py_int_value(v: Option<&Value>) -> Option<i64> {
    match v {
        None | Some(Value::Null) | Some(Value::Bool(false)) => Some(0),
        Some(Value::Bool(true)) => Some(1),
        Some(Value::Number(n)) => match n.as_i64() {
            Some(i) => Some(i),
            None => n
                .as_f64()
                .filter(|f| f.is_finite() && f.abs() < 9.2e18)
                .map(|f| f.trunc() as i64),
        },
        Some(Value::String(s)) if s.is_empty() => Some(0),
        Some(Value::String(s)) => py_int_str(s),
        Some(Value::Array(a)) if a.is_empty() => Some(0),
        Some(Value::Object(o)) if o.is_empty() => Some(0),
        Some(_) => None,
    }
}

/// `float(v or 0.0)` on a JSON value; `None` where Python raises.
fn py_float_value(v: Option<&Value>) -> Option<f64> {
    match v {
        None | Some(Value::Null) | Some(Value::Bool(false)) => Some(0.0),
        Some(Value::Bool(true)) => Some(1.0),
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) if s.is_empty() => Some(0.0),
        Some(Value::String(s)) => py_float(s),
        Some(Value::Array(a)) if a.is_empty() => Some(0.0),
        Some(Value::Object(o)) if o.is_empty() => Some(0.0),
        Some(_) => None,
    }
}

/// Python's `min(a, b)`: `a` unless `b < a` (a NaN `b` never wins).
fn py_min(a: f64, b: f64) -> f64 {
    if b < a {
        b
    } else {
        a
    }
}

/// Python's `max(a, b)`: `a` unless `b > a`.
fn py_max(a: f64, b: f64) -> f64 {
    if b > a {
        b
    } else {
        a
    }
}

/// Python's `round(x, n)` (correctly rounded, ties to even on the exact value).
fn py_round(x: f64, digits: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.digits$}").parse().unwrap_or(x)
}

/// `round((time.monotonic() - t0) * 1000.0)`.
fn elapsed_ms(t0: Instant) -> i64 {
    (t0.elapsed().as_secs_f64() * 1000.0).round_ties_even() as i64
}

/// `json.dumps(v)`: `", "` and `": "` separators, ASCII only, Python's float repr.
pub fn py_dumps(v: &Value) -> String {
    let mut out = String::new();
    dump_into(v, &mut out);
    out
}

fn dump_into(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if n.is_f64() {
                out.push_str(&eve_spend::py_float_repr(n.as_f64().unwrap_or(0.0)));
            } else {
                out.push_str(&n.to_string());
            }
        }
        Value::String(s) => dump_str(s, out),
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                dump_into(x, out);
            }
            out.push(']');
        }
        Value::Object(m) => {
            out.push('{');
            for (i, (k, x)) in m.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                dump_str(k, out);
                out.push_str(": ");
                dump_into(x, out);
            }
            out.push('}');
        }
    }
}

fn dump_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `os.path.dirname`.
fn dirname(p: &str) -> String {
    match p.rfind('/') {
        None => String::new(),
        Some(i) => {
            let head = &p[..=i];
            if head.chars().all(|c| c == '/') {
                head.to_string()
            } else {
                head.trim_end_matches('/').to_string()
            }
        }
    }
}

/// `os.confstr("CS_PATH")` (macOS: `/usr/bin:/bin:/usr/sbin:/sbin`), else `os.defpath`.
fn cs_path() -> String {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let mut buf = vec![0u8; 1024];
        // SAFETY: `buf` is writable for `buf.len()` bytes; confstr writes at most that many, NUL-terminated.
        let n = unsafe { libc::confstr(libc::_CS_PATH, buf.as_mut_ptr().cast(), buf.len()) };
        if n > 0 && n <= buf.len() {
            buf.truncate(n - 1);
            if let Ok(s) = String::from_utf8(buf) {
                return s;
            }
        }
    }
    "/bin:/usr/bin".to_string()
}

/// Python's `posixpath.expanduser`: `~` and `~/...` from `home` (`$HOME`; when unset, the password database),
/// `~name/...` from the password database; an unknown user leaves the path as it is.
fn py_expanduser(raw: &str, home: Option<String>) -> String {
    let Some(rest) = raw.strip_prefix('~') else {
        return raw.to_string();
    };
    let i = rest.find('/').unwrap_or(rest.len());
    let (name, tail) = rest.split_at(i);
    let userhome = if name.is_empty() {
        match home {
            Some(h) => h,
            None => match pw_dir(None) {
                Some(h) => h,
                None => return raw.to_string(),
            },
        }
    } else {
        match pw_dir(Some(name)) {
            Some(h) => h,
            None => return raw.to_string(),
        }
    };
    let joined = format!("{}{tail}", userhome.trim_end_matches('/'));
    if joined.is_empty() {
        "/".to_string()
    } else {
        joined
    }
}

/// The home folder of user `name` (`None`: this process's user) from the password database.
fn pw_dir(name: Option<&str>) -> Option<String> {
    use std::ffi::CStr;
    let cname = match name {
        Some(n) => Some(CString::new(n).ok()?),
        None => None,
    };
    let mut buf = vec![0 as libc::c_char; 16 * 1024];
    // SAFETY: zeroed passwd is a valid out-parameter; the *_r calls write only into `pwd` and `buf`.
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut out: *mut libc::passwd = std::ptr::null_mut();
    let rc = unsafe {
        match &cname {
            Some(c) => libc::getpwnam_r(c.as_ptr(), &mut pwd, buf.as_mut_ptr(), buf.len(), &mut out),
            None => libc::getpwuid_r(libc::getuid(), &mut pwd, buf.as_mut_ptr(), buf.len(), &mut out),
        }
    };
    if rc != 0 || out.is_null() || pwd.pw_dir.is_null() {
        return None;
    }
    // SAFETY: pw_dir points into `buf`, NUL-terminated, alive for this scope.
    let dir = unsafe { CStr::from_ptr(pwd.pw_dir) };
    Some(dir.to_string_lossy().into_owned())
}

/// `os.access(p, os.X_OK)`.
fn access_x(p: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    let Ok(c) = CString::new(p.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `c` is a valid NUL-terminated path for the duration of the call.
    unsafe { libc::access(c.as_ptr(), libc::X_OK) == 0 }
}

/// An OSError's `strerror`, else its class name.
fn strerror(e: &std::io::Error) -> String {
    if e.raw_os_error().is_some() {
        let text = e.to_string();
        match text.rfind(" (os error ") {
            Some(i) => text[..i].to_string(),
            None => text,
        }
    } else {
        os_error_name(e).to_string()
    }
}

/// The Python class an `OSError` would have.
fn os_error_name(e: &std::io::Error) -> &'static str {
    use std::io::ErrorKind as K;
    match e.kind() {
        K::NotFound => "FileNotFoundError",
        K::PermissionDenied => "PermissionError",
        K::AlreadyExists => "FileExistsError",
        K::IsADirectory => "IsADirectoryError",
        K::NotADirectory => "NotADirectoryError",
        _ => "OSError",
    }
}

/// A process's return code as `subprocess` gives it: the exit code, or minus the signal that killed it.
fn exit_code(s: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt as _;
    s.code()
        .or_else(|| s.signal().map(|sig| -sig))
        .unwrap_or(-1)
}

fn lossy(b: Option<&[u8]>) -> String {
    String::from_utf8_lossy(b.unwrap_or_default()).into_owned()
}

#[cfg(test)]
mod tests;
