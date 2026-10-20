//! macOS launchd backend for the daemon auto-start service. Port of `_service_launchd.py`.
//!
//! Writes a user-level LaunchAgent at `~/Library/LaunchAgents/<LABEL>.plist` that runs `eve-bridge daemon` at
//! login and keeps it alive across crashes. Logs stdout/stderr to `~/Library/Logs/eve-bridge.log`.
//!
//! eve lite (EVE_PROFILE=lite, docs/plans/eve-lite.md M8) has a unit of its own, `com.github.eve-bridge.lite`,
//! written by [`Launchd::install_lite`] and logging to `~/Library/Logs/eve-lite.log`. Full eve's plists are built
//! exactly as before.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use eve_claude_home::{daemon_config_dirs_in, HomeDirError, HomeEnv, MULTI_ENV};
use eve_profile::{LITE, PROFILE_ENV};

use crate::host::{os_error_name, path_str, py_splitlines, py_strip, Host, Launch, SystemHost, PATHSEP};
use crate::plistxml::{self, Value};

pub const NAME: &str = "launchd";
pub const LABEL: &str = "com.github.eve-bridge.daemon";

// Second, optional unit: the desktop notes widget (notes_widget.py). Its own label so it can be installed/removed
// independently of the daemon, and KeepAlive off — a widget the user quit from its context menu must stay quit.
pub const WIDGET_LABEL: &str = "com.github.eve-bridge.notes-widget";

// eve lite's unit (docs/plans/eve-lite.md M8). A label of its own, so installing it never replaces full eve's. Its
// environment pins the profile: the env file only fills gaps (envfile.py), so this login item runs eve lite whatever
// the env file says. launchd starts it again only after a failure exit, LITE_THROTTLE_SECS later: a sign-in that
// saved a credential exits 75 and comes back into it, while a stop, or full eve installed for this user, exits 0 and
// stays down (lite.py). Its paths are read from HOME when they are used, not fixed at import as full eve's are.
pub const LITE_LABEL: &str = "com.github.eve-bridge.lite";
pub const LITE_LOG_NAME: &str = "eve-lite.log";
/// launchd's own default: eve lite needs nothing from Homebrew.
pub const LITE_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";
pub const LITE_THROTTLE_SECS: i64 = 5;
/// 63 in the plist: what the daemon creates is the user's alone.
pub const LITE_UMASK: i64 = 0o077;

/// Full eve's default PATH. launchd starts with an empty one, and some bleak paths shell out.
const FULL_PATH: &str = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";

/// Full eve's paths, which Python fixes from `Path.home()` when the module is imported (`PLIST_PATH`, `LOG_PATH`,
/// `WIDGET_PLIST_PATH`, `WIDGET_LOG_PATH`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FullPaths {
    pub plist_path: PathBuf,
    pub log_path: PathBuf,
    pub widget_plist_path: PathBuf,
    pub widget_log_path: PathBuf,
}

impl FullPaths {
    /// The paths under `home`.
    pub fn under(home: &Path) -> FullPaths {
        let agents = home.join("Library").join("LaunchAgents");
        let logs = home.join("Library").join("Logs");
        FullPaths {
            plist_path: agents.join(format!("{LABEL}.plist")),
            log_path: logs.join("eve-bridge.log"),
            widget_plist_path: agents.join(format!("{WIDGET_LABEL}.plist")),
            widget_log_path: logs.join("eve-bridge-notes-widget.log"),
        }
    }

    /// The paths as this process fixed them the first time they were asked for (Python's import).
    pub fn process() -> Result<FullPaths, HomeDirError> {
        static FULL: LazyLock<Result<FullPaths, HomeDirError>> =
            LazyLock::new(|| HomeEnv::from_process().home_dir().map(|home| FullPaths::under(&home)));
        FULL.clone()
    }
}

/// The launchd backend: the module's state (its import-time paths and label), the environment it reads when it
/// builds a unit (`HOME`, the Claude homes), the program the unit starts, and the process boundary.
#[derive(Clone)]
pub struct Launchd {
    /// `HOME`, `CLAUDE_CONFIG_DIR` and `EVE_CLAUDE_CONFIG_DIRS` as they are now.
    pub env: HomeEnv,
    /// Full eve's paths, fixed at import.
    pub full: FullPaths,
    /// Full eve's daemon label ([`LABEL`]).
    pub label: String,
    /// What `ProgramArguments` starts (`sys.executable` in Python).
    pub launch: Launch,
    pub host: Arc<dyn Host>,
}

impl std::fmt::Debug for Launchd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Launchd")
            .field("env", &self.env)
            .field("full", &self.full)
            .field("label", &self.label)
            .field("launch", &self.launch)
            .finish_non_exhaustive()
    }
}

impl Launchd {
    /// This process: its environment now, full eve's paths as first fixed, this binary, the real host.
    pub fn from_process() -> Result<Launchd, HomeDirError> {
        Ok(Launchd {
            env: HomeEnv::from_process(),
            full: FullPaths::process()?,
            label: LABEL.to_string(),
            launch: Launch::current(),
            host: Arc::new(SystemHost),
        })
    }

    /// `Path.home()`, read now.
    pub fn home(&self) -> Result<PathBuf, HomeDirError> {
        self.env.home_dir()
    }

    /// `_base_plist`: shared LaunchAgent skeleton for every eve-bridge unit.
    ///
    /// `ProgramArguments` uses the program that's running *this* install command, so a user who installs from
    /// inside the project venv gets a service pointing at that venv. No need for a separate executable path.
    ///
    /// `ProcessType = Interactive` tells launchd this agent runs in the user's GUI session; required for
    /// CoreBluetooth (BLE) access and for AppKit windows on macOS.
    pub fn base_plist(&self, label: &str, subcommand: &str, keep_alive: bool, log_path: &Path) -> anyhow::Result<Value> {
        let log = path_str(log_path)?.to_string();
        let home = self.home()?;
        Ok(Value::Dict(vec![
            ("Label".into(), Value::s(label)),
            (
                "ProgramArguments".into(),
                Value::Array(self.launch.argv(subcommand).into_iter().map(Value::String).collect()),
            ),
            ("RunAtLoad".into(), Value::Bool(true)),
            ("KeepAlive".into(), Value::Bool(keep_alive)),
            ("ProcessType".into(), Value::s("Interactive")),
            ("StandardOutPath".into(), Value::s(log.clone())),
            ("StandardErrorPath".into(), Value::s(log)),
            (
                "EnvironmentVariables".into(),
                Value::Dict(vec![
                    ("HOME".into(), Value::s(path_str(&home)?)),
                    // Keep a reasonable default PATH — launchd starts with an empty one, and some bleak paths
                    // shell out.
                    ("PATH".into(), Value::s(FULL_PATH)),
                ]),
            ),
        ]))
    }

    /// `_build_plist`: render the daemon plist as XML bytes. See [`Launchd::base_plist`].
    pub fn build_plist(&self, serial_port: Option<&str>) -> anyhow::Result<Vec<u8>> {
        let mut plist = self.base_plist(&self.label, "daemon", true, &self.full.log_path)?;
        let mut extra: Vec<(&str, String)> = Vec::new();
        if let Some(port) = serial_port.filter(|p| !p.is_empty()) {
            // cli.py's daemon subcommand defaults --serial-port from this env var, so the service uses USB serial
            // instead of BLE.
            extra.push(("EVE_SERIAL_PORT", port.to_string()));
        }
        // launchd starts the daemon outside any Claude Code session, so it can never inherit a per-session
        // $CLAUDE_CONFIG_DIR. Bake in the homes to serve at install time — otherwise the daemon tails only
        // ~/.claude and sessions run by a wrapper (era-code) report no tokens and no entries.
        let dirs = daemon_config_dirs_in(&self.env, None)?;
        if dirs != [self.home()?.join(".claude")] {
            let joined = dirs.iter().map(|d| path_str(d)).collect::<anyhow::Result<Vec<_>>>()?.join(PATHSEP);
            extra.push((MULTI_ENV, joined));
        }
        if let Some(env) = plist.get_mut("EnvironmentVariables") {
            for (key, value) in extra {
                env.set(key, Value::s(value));
            }
        }
        Ok(plistxml::dumps(&plist)?)
    }

    /// `_build_widget_plist`: render the notes-widget plist. Same program and env as the daemon unit; no
    /// daemon-only variables (serial port, hotkey, Claude homes).
    pub fn build_widget_plist(&self) -> anyhow::Result<Vec<u8>> {
        let plist = self.base_plist(WIDGET_LABEL, "notes-widget", false, &self.full.widget_log_path)?;
        Ok(plistxml::dumps(&plist)?)
    }

    /// `_load`: unload first so an idempotent re-install picks up any new program path, then `load -w`.
    pub fn load(&self, plist_path: &Path) -> io::Result<i32> {
        let p = plist_path.to_string_lossy().into_owned();
        self.host.run(&["launchctl".into(), "unload".into(), p.clone()])?;
        let result = self.host.run(&["launchctl".into(), "load".into(), "-w".into(), p])?;
        if result.returncode != 0 {
            self.host.eprint(&format!(
                "launchctl load failed ({}): {}",
                result.returncode,
                py_strip(&result.stderr)
            ));
            return Ok(2);
        }
        Ok(0)
    }

    fn no_launchctl(&self) -> bool {
        if self.host.which("launchctl").is_none() {
            self.host.eprint("eve-bridge: `launchctl` not found on PATH");
            return true;
        }
        false
    }

    /// `install`: write full eve's daemon plist and load it.
    pub fn install(&self, serial_port: Option<&str>) -> anyhow::Result<i32> {
        if self.no_launchctl() {
            return Ok(2);
        }
        let (plist, log) = (&self.full.plist_path, &self.full.log_path);
        create_parent(plist)?;
        create_parent(log)?;
        fs::write(plist, self.build_plist(serial_port)?)?;

        let rc = self.load(plist)?;
        if rc != 0 {
            return Ok(rc);
        }
        self.host.print(&format!("installed: {}", plist.display()));
        self.host.print(&format!("logs at:   {}", log.display()));
        self.host.print("daemon will start on your next login (and is starting now).");
        Ok(0)
    }

    /// `uninstall`: unload full eve's daemon and remove its plist.
    pub fn uninstall(&self) -> anyhow::Result<i32> {
        let plist = &self.full.plist_path;
        if !plist.exists() {
            self.host.print("service not installed; nothing to do");
            return Ok(0);
        }
        self.host.run(&["launchctl".into(), "unload".into(), plist.to_string_lossy().into_owned()])?;
        fs::remove_file(plist)?;
        self.host.print(&format!("removed: {}", plist.display()));
        Ok(0)
    }

    /// `install_widget`: write the notes widget's plist and load it.
    pub fn install_widget(&self) -> anyhow::Result<i32> {
        if self.no_launchctl() {
            return Ok(2);
        }
        let (plist, log) = (&self.full.widget_plist_path, &self.full.widget_log_path);
        create_parent(plist)?;
        create_parent(log)?;
        fs::write(plist, self.build_widget_plist()?)?;

        let rc = self.load(plist)?;
        if rc != 0 {
            return Ok(rc);
        }
        self.host.print(&format!("installed: {}", plist.display()));
        self.host.print(&format!("logs at:   {}", log.display()));
        self.host.print("notes widget will show on your next login (and is showing now).");
        Ok(0)
    }

    /// `uninstall_widget`.
    pub fn uninstall_widget(&self) -> anyhow::Result<i32> {
        let plist = &self.full.widget_plist_path;
        if !plist.exists() {
            self.host.print("notes widget not installed; nothing to do");
            return Ok(0);
        }
        self.host.run(&["launchctl".into(), "unload".into(), plist.to_string_lossy().into_owned()])?;
        fs::remove_file(plist)?;
        self.host.print(&format!("removed: {}", plist.display()));
        Ok(0)
    }

    /// `is_installed`: full eve's daemon plist exists.
    pub fn is_installed(&self) -> bool {
        self.full.plist_path.exists()
    }

    /// `is_loaded`: true iff launchctl reports the agent currently loaded (the label anywhere in a line).
    pub fn is_loaded(&self) -> io::Result<bool> {
        if self.host.which("launchctl").is_none() {
            return Ok(false);
        }
        let result = self.host.run(&["launchctl".into(), "list".into()])?;
        if result.returncode != 0 {
            return Ok(false);
        }
        Ok(py_splitlines(&result.stdout).iter().any(|line| line.contains(self.label.as_str())))
    }

    /// `unit_path`.
    pub fn unit_path(&self) -> &Path {
        &self.full.plist_path
    }

    /// `log_path`.
    pub fn log_path(&self) -> &Path {
        &self.full.log_path
    }

    // ---- eve lite's unit --------------------------------------------------------------------------------------

    /// `agent_path`: where this user's LaunchAgent `label` lives, read from HOME now.
    pub fn agent_path(&self, label: &str) -> Result<PathBuf, HomeDirError> {
        Ok(self.home()?.join("Library").join("LaunchAgents").join(format!("{label}.plist")))
    }

    /// `lite_plist_path`.
    pub fn lite_plist_path(&self) -> Result<PathBuf, HomeDirError> {
        self.agent_path(LITE_LABEL)
    }

    /// `lite_log_path`.
    pub fn lite_log_path(&self) -> Result<PathBuf, HomeDirError> {
        Ok(self.home()?.join("Library").join("Logs").join(LITE_LOG_NAME))
    }

    /// `_build_lite_plist`: eve lite's plist (spec M8). Not [`Launchd::base_plist`]: launchd restarts eve lite only
    /// after a failure exit and only after LITE_THROTTLE_SECS, and starts it with the profile, owner-only files
    /// (Umask), launchd's own PATH and no user site-packages (PYTHONNOUSERSITE: nothing outside eve lite's venv is
    /// imported). `ProgramArguments` is the program running this install, eve lite's own when the installer runs it.
    pub fn build_lite_plist(&self) -> anyhow::Result<Vec<u8>> {
        let log = path_str(&self.lite_log_path()?)?.to_string();
        let home = self.home()?;
        let plist = Value::Dict(vec![
            ("Label".into(), Value::s(LITE_LABEL)),
            (
                "ProgramArguments".into(),
                Value::Array(self.launch.argv("daemon").into_iter().map(Value::String).collect()),
            ),
            ("RunAtLoad".into(), Value::Bool(true)),
            ("KeepAlive".into(), Value::Dict(vec![("SuccessfulExit".into(), Value::Bool(false))])),
            ("ThrottleInterval".into(), Value::Integer(LITE_THROTTLE_SECS)),
            ("ProcessType".into(), Value::s("Interactive")),
            ("Umask".into(), Value::Integer(LITE_UMASK)),
            ("StandardOutPath".into(), Value::s(log.clone())),
            ("StandardErrorPath".into(), Value::s(log)),
            (
                "EnvironmentVariables".into(),
                Value::Dict(vec![
                    ("HOME".into(), Value::s(path_str(&home)?)),
                    ("PATH".into(), Value::s(LITE_PATH)),
                    (PROFILE_ENV.into(), Value::s(LITE)),
                    ("PYTHONNOUSERSITE".into(), Value::s("1")),
                ]),
            ),
        ]);
        Ok(plistxml::dumps(&plist)?)
    }

    /// `install_lite`: `eve-bridge install --service` under EVE_PROFILE=lite: write eve lite's plist and (re)load
    /// it, so eve lite starts now and at every login; run again, it picks up a new program path (an update).
    /// Refused while full eve's agent is installed for this user: full eve and eve lite keep their data in the
    /// same ~/.config/eve-bridge, so a Mac account has one of them (install-lite.sh refuses it too, and eve lite's
    /// daemon stays down beside full eve).
    pub fn install_lite(&self) -> anyhow::Result<i32> {
        if self.no_launchctl() {
            return Ok(2);
        }
        let full = self.agent_path(&self.label)?;
        if full.exists() {
            self.host.eprint(&format!(
                "eve-bridge: full eve's login service is installed for this user ({}). Full eve and eve lite \
                 share ~/.config/eve-bridge, so a Mac account has one of them. Nothing was changed.",
                full.display()
            ));
            return Ok(2);
        }
        let (path, log) = (self.lite_plist_path()?, self.lite_log_path()?);
        let written: io::Result<()> = (|| {
            create_parent(&path)?;
            create_parent(&log)?;
            Ok(())
        })();
        let written = match written {
            // The plist is built between the folders and the write, as Python evaluates it; its own errors are
            // not OSErrors and are not caught.
            Ok(()) => write_plist(&path, &self.build_lite_plist()?),
            Err(e) => Err(e),
        };
        if let Err(e) = written {
            let folder = path.parent().unwrap_or(Path::new(""));
            self.host.eprint(&format!(
                "eve-bridge: eve lite's login service could not be written to {} ({}).",
                folder.display(),
                os_error_name(&e)
            ));
            return Ok(2);
        }

        let rc = self.load(&path)?;
        if rc != 0 {
            return Ok(rc);
        }
        self.host.print(&format!("installed: {}", path.display()));
        self.host.print(&format!("logs at:   {}", log.display()));
        self.host.print("eve lite will start on your next login (and is starting now).");
        Ok(0)
    }

    /// `uninstall_lite`: stop eve lite and remove its plist. It is stopped by its label (`launchctl bootout`, as
    /// install-lite.sh --uninstall does), which works whether or not the file is still there; `unload` by the file
    /// when that fails.
    pub fn uninstall_lite(&self) -> anyhow::Result<i32> {
        let path = self.lite_plist_path()?;
        let loaded = self.lite_loaded()?;
        if !loaded && !path.exists() {
            self.host.print("eve lite's login service is not installed; nothing to do");
            return Ok(0);
        }
        if loaded {
            let target = format!("gui/{}/{LITE_LABEL}", self.host.getuid()?);
            let done = self.host.run(&["launchctl".into(), "bootout".into(), target])?;
            if done.returncode != 0 && path.exists() {
                self.host.run(&["launchctl".into(), "unload".into(), path.to_string_lossy().into_owned()])?;
            }
            self.host.print("stopped eve lite");
        }
        if path.exists() {
            fs::remove_file(&path)?;
            self.host.print(&format!("removed: {}", path.display()));
        }
        Ok(0)
    }

    /// `lite_installed`: eve lite's plist exists under HOME now.
    pub fn lite_installed(&self) -> Result<bool, HomeDirError> {
        Ok(self.lite_plist_path()?.exists())
    }

    /// `lite_loaded`: true iff launchctl lists eve lite's agent: running, or loaded and waiting to start again.
    pub fn lite_loaded(&self) -> io::Result<bool> {
        if self.host.which("launchctl").is_none() {
            return Ok(false);
        }
        let result = self.host.run(&["launchctl".into(), "list".into()])?;
        if result.returncode != 0 {
            return Ok(false);
        }
        // "PID\tStatus\tLabel" per line: the label is the last column, matched whole
        Ok(py_splitlines(&result.stdout)
            .iter()
            .any(|line| py_strip(line.rsplit('\t').next().unwrap_or(line)) == LITE_LABEL))
    }

    /// `installed_lite_plist`: eve lite's plist as installed, parsed; None when there is none, or it does not
    /// parse (missing, unreadable or not a plist: there is no usable one).
    pub fn installed_lite_plist(&self) -> Option<plist::Dictionary> {
        let path = self.lite_plist_path().ok()?;
        let data = fs::read(path).ok()?;
        match plist::Value::from_reader(io::Cursor::new(data)).ok()? {
            plist::Value::Dictionary(d) => Some(d),
            _ => None,
        }
    }
}

/// `path.parent.mkdir(parents=True, exist_ok=True)`.
fn create_parent(path: &Path) -> io::Result<()> {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => fs::create_dir_all(parent),
        _ => Ok(()),
    }
}

/// `_write_plist`: `data` into `path` through a temporary file in the same folder and one rename, so launchd never
/// reads half a plist; 0644, as LaunchAgents are (it holds no secret).
pub fn write_plist(path: &Path, data: &[u8]) -> io::Result<()> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    // The temporary file is removed on any failure (NamedTempFile's drop), as Python's except clause does.
    let mut tmp = tempfile::Builder::new().prefix(&format!(".{name}.")).suffix(".tmp").tempfile_in(dir)?;
    tmp.write_all(data)?;
    tmp.flush()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o644))?;
    }
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}
