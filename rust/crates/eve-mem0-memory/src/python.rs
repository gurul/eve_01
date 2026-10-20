//! The boundary: mem0's `Memory` in a helper Python process.
//!
//! mem0 (mem0ai) is Python only, and what it keeps on disk is in Python's own formats (Qdrant's local mode, its
//! SQLite history), so a Rust daemon reading and writing the same store has to run mem0 itself. The helper
//! (`src/mem0_helper.py`, passed with `-c`) is `open_mem0` and its client wrappers, verbatim; each request is one
//! call on the opened Memory, one JSON object per line each way. The config it is given is [`mem0_config`]'s, the
//! OpenRouter ids of its two models come from `eve_openrouter::model`, and each model call it makes comes back as a
//! spend line that is recorded here through `eve_spend` ([`record_spend`]).

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use eve_spend::Ledger;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use tracing::warn;

use crate::mem0_memory::{mem0_config, path_str, Mem0Backend, Mem0Config, Mem0Error, EXTRACT_MAX_OUTPUT_TOKENS};

/// The helper's source, run with `python -c`.
pub const HELPER: &str = include_str!("mem0_helper.py");
/// The interpreter that has mem0ai (new; the Python daemon runs mem0 in its own interpreter).
pub const PYTHON_ENV: &str = "EVE_MEM0_PYTHON";
/// How long `available` trusts an answer: a mem0ai installed (or removed) while the daemon runs is seen this soon.
const AVAILABLE_TTL: Duration = Duration::from_secs(60);
/// How long a closed helper is given to exit on its own before it is killed.
const EXIT_GRACE: Duration = Duration::from_secs(5);

/// The Python that has mem0ai: EVE_MEM0_PYTHON, else the bridge checkout's venv, else `python3`.
pub fn default_python() -> PathBuf {
    if let Some(p) = std::env::var_os(PYTHON_ENV).filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    let venv = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/.venv/bin/python3"));
    if venv.exists() {
        return venv;
    }
    PathBuf::from("python3")
}

const FIND_SPEC: &str = "import importlib.util, sys\n\
try:\n    ok = importlib.util.find_spec('mem0') is not None\n\
except (ImportError, ValueError):\n    ok = False\n\
sys.exit(0 if ok else 1)\n";

static AVAILABLE: LazyLock<Mutex<HashMap<PathBuf, (Instant, bool)>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Whether mem0ai is installed for [`default_python`]. Checked without importing it (importing starts its
/// telemetry).
pub fn available() -> bool {
    available_with(&default_python())
}

/// Whether `python` can find mem0 (`importlib.util.find_spec("mem0")`); remembered for a minute.
pub fn available_with(python: &Path) -> bool {
    if let Some((at, ok)) = AVAILABLE.lock().get(python) {
        if at.elapsed() < AVAILABLE_TTL {
            return *ok;
        }
    }
    let ok = Command::new(python)
        .arg("-c")
        .arg(FIND_SPEC)
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    AVAILABLE.lock().insert(python.to_path_buf(), (Instant::now(), ok));
    ok
}

/// One model call the helper reports, recorded in the daily spend ledger as `meter` does: fact extraction as a Chat
/// Completions reply (`spend.record_chat_completion(spend.MEMORY, reply, model=m or cfg.model)`) and embeddings as
/// an embeddings reply (`spend.record_embedding(spend.MEMORY, m or cfg.embed_model, reply)`).
pub fn record_spend(ledger: &Ledger, cfg: &Mem0Config, kind: &str, model: &str, reply: &Value) {
    match kind {
        "chat" => {
            let m = if model.is_empty() { cfg.model.as_str() } else { model };
            ledger.record_chat_completion(eve_spend::MEMORY, reply, m, eve_openrouter::PROVIDER);
        }
        "embedding" => {
            let m = if model.is_empty() { cfg.embed_model.as_str() } else { model };
            ledger.record_embedding(eve_spend::MEMORY, m, reply, eve_openrouter::PROVIDER);
        }
        other => warn!("mem0: a spend line of an unknown kind ({other}) was not recorded"),
    }
}

struct Helper {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl Helper {
    /// The next protocol line that is not a log or spend line; those are handled on the way.
    fn read(&mut self, cfg: &Mem0Config, ledger: &Ledger) -> anyhow::Result<Map<String, Value>> {
        loop {
            let mut line = String::new();
            let n = self.stdout.read_line(&mut line)?;
            if n == 0 {
                let status = self.child.try_wait().ok().flatten();
                return Err(Mem0Error::new(
                    "RuntimeError",
                    format!("mem0 helper exited{}", status.map(|s| format!(" ({s})")).unwrap_or_default()),
                )
                .into());
            }
            if line.trim().is_empty() {
                continue;
            }
            let Some(Value::Object(obj)) = eve_transcripts::py::loads(line.trim_end()) else {
                return Err(Mem0Error::new("RuntimeError", "mem0 helper sent a bad line").into());
            };
            if obj.contains_key("log") {
                let message = obj.get("message").and_then(Value::as_str).unwrap_or_default();
                warn!("{message}");
                continue;
            }
            if let Some(kind) = obj.get("spend").and_then(Value::as_str) {
                let model = obj.get("model").and_then(Value::as_str).unwrap_or_default();
                record_spend(ledger, cfg, kind, model, obj.get("reply").unwrap_or(&Value::Null));
                continue;
            }
            return Ok(obj);
        }
    }

    fn ask(&mut self, request: &Value, cfg: &Mem0Config, ledger: &Ledger) -> anyhow::Result<Map<String, Value>> {
        let stdin = self.stdin.as_mut().ok_or_else(|| Mem0Error::new("RuntimeError", "mem0 helper is closed"))?;
        writeln!(stdin, "{request}")?;
        stdin.flush()?;
        self.read(cfg, ledger)
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        // Closing stdin ends the helper's loop; mem0 then closes its store itself. Killed only if it hangs.
        drop(self.stdin.take());
        let deadline = Instant::now() + EXIT_GRACE;
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The exception a helper reply reports, `{"kind", "error"}`.
fn reported(reply: &Map<String, Value>) -> Mem0Error {
    Mem0Error::new(
        reply.get("kind").and_then(Value::as_str).unwrap_or("RuntimeError"),
        reply.get("error").and_then(Value::as_str).unwrap_or_default(),
    )
}

/// mem0's opened Memory, in the helper process. Opening it is spawning the helper (mem0's import, its telemetry
/// switch, `Memory.from_config`, the OpenRouter clients and the meter); a helper that died is opened again on the
/// next call.
pub struct PythonMem0 {
    cfg: Mem0Config,
    python: PathBuf,
    env: Vec<(OsString, OsString)>,
    ledger: Ledger,
    helper: Option<Helper>,
}

impl std::fmt::Debug for PythonMem0 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PythonMem0").field("cfg", &self.cfg).field("python", &self.python).finish_non_exhaustive()
    }
}

impl PythonMem0 {
    /// Open mem0 for `cfg` in `python`, with `env` added to the helper's environment (the daemon's is inherited),
    /// its model calls recorded in `ledger`. Errs as `open_mem0` raises, by the Python class name.
    pub fn open(
        cfg: Mem0Config,
        python: impl Into<PathBuf>,
        env: Vec<(OsString, OsString)>,
        ledger: Ledger,
    ) -> anyhow::Result<Self> {
        let mut mem = PythonMem0 { cfg, python: python.into(), env, ledger, helper: None };
        mem.helper = Some(mem.spawn()?);
        Ok(mem)
    }

    /// What the helper is started with: mem0's config, its folders, and the OpenRouter endpoint and model ids.
    pub fn init(&self) -> Value {
        let cfg = &self.cfg;
        let mut models = Map::new();
        for name in [&cfg.model, &cfg.embed_model] {
            models.insert(name.clone(), Value::String(eve_openrouter::model(name)));
        }
        let headers: Map<String, Value> =
            eve_openrouter::HEADERS.iter().map(|(k, v)| (k.to_string(), Value::String(v.to_string()))).collect();
        json!({
            "config": mem0_config(cfg),
            "home": path_str(&cfg.home),
            "mem0_dir": path_str(&cfg.home.join("home")),
            "embedder": cfg.embedder,
            "key_env": eve_openrouter::KEY_ENV,
            "base_url": eve_openrouter::BASE_URL,
            "headers": headers,
            "models": models,
            "extract_max_output_tokens": EXTRACT_MAX_OUTPUT_TOKENS,
        })
    }

    fn spawn(&self) -> anyhow::Result<Helper> {
        let mut cmd = Command::new(&self.python);
        cmd.arg("-c").arg(HELPER).arg(self.init().to_string());
        cmd.envs(self.env.iter().map(|(k, v)| (k, v)));
        // The script's own directory is not on sys.path; a neutral cwd keeps a planted module from loading.
        cmd.current_dir("/");
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());
        let mut child = cmd.spawn().map_err(|e| {
            let kind = if e.kind() == std::io::ErrorKind::NotFound { "FileNotFoundError" } else { "OSError" };
            Mem0Error::new(kind, format!("{}: {e}", self.python.display()))
        })?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Mem0Error::new("OSError", "mem0 helper has no pipes").into());
        };
        let mut helper = Helper { child, stdin: Some(stdin), stdout: BufReader::new(stdout) };
        let status = helper.read(&self.cfg, &self.ledger)?;
        match status.get("status").and_then(Value::as_str) {
            Some("ready") => Ok(helper),
            _ => Err(reported(&status).into()),
        }
    }

    /// One call on the Memory: its result, or the exception it raised.
    pub fn call(&mut self, request: Value) -> anyhow::Result<Value> {
        if self.helper.is_none() {
            // The helper died on an earlier call: open again rather than fail every call after.
            self.helper = Some(self.spawn()?);
        }
        let Some(helper) = self.helper.as_mut() else {
            return Err(Mem0Error::new("RuntimeError", "mem0 helper is not running").into());
        };
        let mut reply = match helper.ask(&request, &self.cfg, &self.ledger) {
            Ok(reply) => reply,
            Err(e) => {
                self.helper = None;
                return Err(e);
            }
        };
        match reply.remove("result") {
            Some(result) => Ok(result),
            None => Err(reported(&reply).into()),
        }
    }
}

impl Mem0Backend for PythonMem0 {
    fn search(&mut self, query: &str, filters: &Value, top_k: usize) -> anyhow::Result<Value> {
        self.call(json!({"op": "search", "query": query, "filters": filters, "top_k": top_k}))
    }

    fn get_all(&mut self, filters: &Value, top_k: usize) -> anyhow::Result<Value> {
        self.call(json!({"op": "get_all", "filters": filters, "top_k": top_k}))
    }

    fn add(&mut self, messages: &Value, user_id: &str, metadata: &Value) -> anyhow::Result<Value> {
        self.call(json!({"op": "add", "messages": messages, "user_id": user_id, "metadata": metadata}))
    }

    fn delete(&mut self, memory_id: &str) -> anyhow::Result<Value> {
        self.call(json!({"op": "delete", "memory_id": memory_id}))
    }

    fn embed(&mut self, text: &str, memory_action: &str) -> anyhow::Result<bool> {
        let out = self.call(json!({"op": "embed", "text": text, "memory_action": memory_action}))?;
        Ok(out.get("embedded").and_then(Value::as_bool).unwrap_or(false))
    }
}
