//! Linux systemd user-unit backend for the daemon auto-start service. Port of `_service_systemd.py`.
//!
//! Writes a user-level unit at `~/.config/systemd/user/<UNIT_NAME>` that runs `eve-bridge daemon` and keeps it
//! alive across crashes. Stdout and stderr go through systemd-journald — view them with
//! `journalctl --user -u eve-bridge.service`.
//!
//! By default a user manager exits when the user logs out, so the daemon stops with it. Enable lingering
//! (`loginctl enable-linger $USER`) if you want the daemon to survive logout / start at boot.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use eve_claude_home::{daemon_config_dirs_in, HomeDirError, HomeEnv, MULTI_ENV};

use crate::host::{path_str, py_strip, Completed, Host, Launch, SystemHost, PATHSEP};

pub const NAME: &str = "systemd";
pub const UNIT_NAME: &str = "eve-bridge.service";

/// `UNIT_PATH` under `home`.
pub fn unit_path_under(home: &Path) -> PathBuf {
    home.join(".config").join("systemd").join("user").join(UNIT_NAME)
}

/// The systemd backend: `UNIT_PATH` (fixed at import in Python), the environment read when the unit is built, the
/// program the unit starts, and the process boundary.
#[derive(Clone)]
pub struct Systemd {
    pub env: HomeEnv,
    pub unit_path: PathBuf,
    pub launch: Launch,
    pub host: Arc<dyn Host>,
}

impl std::fmt::Debug for Systemd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Systemd")
            .field("env", &self.env)
            .field("unit_path", &self.unit_path)
            .field("launch", &self.launch)
            .finish_non_exhaustive()
    }
}

impl Systemd {
    /// This process: its environment now, `UNIT_PATH` as first fixed, this binary, the real host.
    pub fn from_process() -> Result<Systemd, HomeDirError> {
        static UNIT: LazyLock<Result<PathBuf, HomeDirError>> =
            LazyLock::new(|| HomeEnv::from_process().home_dir().map(|h| unit_path_under(&h)));
        Ok(Systemd {
            env: HomeEnv::from_process(),
            unit_path: UNIT.clone()?,
            launch: Launch::current(),
            host: Arc::new(SystemHost),
        })
    }

    /// `_build_unit`: render the .service unit file.
    ///
    /// `ExecStart` uses the program that's running *this* install command — same trick as the macOS plist — so
    /// the unit picks up the install it came from.
    pub fn build_unit(&self, serial_port: Option<&str>) -> anyhow::Result<String> {
        let mut env_line = match serial_port.filter(|p| !p.is_empty()) {
            Some(port) => format!("Environment=\"EVE_SERIAL_PORT={port}\"\n"),
            None => String::new(),
        };
        // See the launchd backend: a service-run daemon can't inherit a per-session $CLAUDE_CONFIG_DIR, so the
        // homes to watch are baked in at install time.
        let dirs = daemon_config_dirs_in(&self.env, None)?;
        if dirs != [self.env.home_dir()?.join(".claude")] {
            let joined = dirs.iter().map(|d| path_str(d)).collect::<anyhow::Result<Vec<_>>>()?.join(PATHSEP);
            env_line.push_str(&format!("Environment=\"{MULTI_ENV}={joined}\"\n"));
        }
        let exec = self.launch.argv("daemon").join(" ");
        Ok(format!(
            "[Unit]\n\
             Description=Claude Code <-> desktop-eve BLE bridge\n\
             After=default.target\n\
             \n\
             [Service]\n\
             Type=simple\n\
             {env_line}\
             ExecStart={exec}\n\
             Restart=on-failure\n\
             RestartSec=5\n\
             \n\
             [Install]\n\
             WantedBy=default.target\n"
        ))
    }

    /// `_systemctl`: `systemctl --user <args>`.
    pub fn systemctl(&self, args: &[&str]) -> io::Result<Completed> {
        let mut argv = vec!["systemctl".to_string(), "--user".to_string()];
        argv.extend(args.iter().map(|a| a.to_string()));
        self.host.run(&argv)
    }

    /// `install`: write the unit, reload systemd and enable it now.
    pub fn install(&self, serial_port: Option<&str>) -> anyhow::Result<i32> {
        if self.host.which("systemctl").is_none() {
            self.host.eprint("eve-bridge: `systemctl` not found on PATH");
            return Ok(2);
        }
        if let Some(parent) = self.unit_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&self.unit_path, self.build_unit(serial_port)?)?;

        let reload_res = self.systemctl(&["daemon-reload"])?;
        if reload_res.returncode != 0 {
            self.host.eprint(&format!(
                "systemctl --user daemon-reload failed ({}): {}",
                reload_res.returncode,
                py_strip(&reload_res.stderr)
            ));
            return Ok(2);
        }

        let enable_res = self.systemctl(&["enable", "--now", UNIT_NAME])?;
        if enable_res.returncode != 0 {
            self.host.eprint(&format!(
                "systemctl --user enable --now failed ({}): {}",
                enable_res.returncode,
                py_strip(&enable_res.stderr)
            ));
            return Ok(2);
        }

        self.host.print(&format!("installed: {}", self.unit_path.display()));
        self.host.print(&format!("logs:      journalctl --user -u {UNIT_NAME}"));
        self.host.print("daemon is running and will start on your next login.");
        self.host.print("tip: run `loginctl enable-linger $USER` if you want it to survive logout / start at boot.");
        Ok(0)
    }

    /// `uninstall`: disable the unit, remove it, reload systemd.
    pub fn uninstall(&self) -> anyhow::Result<i32> {
        if !self.unit_path.exists() {
            self.host.print("service not installed; nothing to do");
            return Ok(0);
        }
        if self.host.which("systemctl").is_some() {
            self.systemctl(&["disable", "--now", UNIT_NAME])?;
        }
        fs::remove_file(&self.unit_path)?;
        if self.host.which("systemctl").is_some() {
            self.systemctl(&["daemon-reload"])?;
        }
        self.host.print(&format!("removed: {}", self.unit_path.display()));
        Ok(0)
    }

    /// `is_installed`.
    pub fn is_installed(&self) -> bool {
        self.unit_path.exists()
    }

    /// `is_loaded`: true iff systemd reports the unit currently active (`is-active` exits 0 when it is).
    pub fn is_loaded(&self) -> io::Result<bool> {
        if self.host.which("systemctl").is_none() {
            return Ok(false);
        }
        Ok(self.systemctl(&["is-active", "--quiet", UNIT_NAME])?.returncode == 0)
    }

    /// `unit_path`.
    pub fn unit_path(&self) -> &Path {
        &self.unit_path
    }

    /// `log_path`: the `journalctl` invocation, since journald is the log store.
    pub fn log_path(&self) -> String {
        format!("journalctl --user -u {UNIT_NAME}")
    }
}
