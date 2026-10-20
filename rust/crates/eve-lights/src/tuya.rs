//! Sylvania Smart+ Wi-Fi: Tuya's local protocol, through tinytuya.
//!
//! Tuya's local protocol (versions 3.1 to 3.5, each with its own framing and crypto) and tinytuya's
//! `BulbDevice` (which data points a bulb of each type uses, and how it encodes colour and level) are what the
//! Python daemon drives, through the `lights` extra. The port keeps that library: each read or change runs a
//! short Python helper that makes the same `BulbDevice` calls and prints one JSON line back. This is a
//! boundary, recorded in PARITY.md. The decisions (what to send for a change, what a reply means) stay here.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::lights::{Change, Driver, Light, LightError, Rgb, State};
use crate::py;

/// a Tuya bulb's colour-temperature percent spans about this
pub const TUYA_KELVIN: (i64, i64) = (2700, 6500);

/// The interpreter that runs the helper: `EVE_LIGHTS_PYTHON`, else `python3`. It needs tinytuya.
pub const PYTHON_ENV: &str = "EVE_LIGHTS_PYTHON";

pub(crate) const NOT_INSTALLED: &str = "tinytuya is not installed: pip install -e \".[lights]\"";

/// The helper. It prints exactly one JSON line on stdout: `{"ok": result}` or `{"error": {"type", "message"}}`;
/// anything tinytuya prints goes to stderr.
const HELPER: &str = r#"
import contextlib, json, sys


def _tuya_ok(reply):
    if isinstance(reply, dict) and "Error" in reply:
        raise ConnectionError(str(reply["Error"]))


def run(tinytuya, req):
    if req["op"] == "scan":
        found = tinytuya.deviceScan(verbose=False, maxretry=req["seconds"], color=False, poll=False)
        return [{"id": str(d.get("gwId") or d.get("id") or ""), "ip": str(d.get("ip") or ip),
                 "version": str(d.get("version") or "3.3")} for ip, d in (found or {}).items()]
    lt = req["light"]
    dev = tinytuya.BulbDevice(lt["id"], lt["ip"] or "Auto", lt["key"], version=float(lt["version"] or 3.3))
    dev.set_socketTimeout(3)
    dev.set_socketRetryLimit(1)
    try:
        if req["op"] == "state":
            s = dev.state()
            if not isinstance(s, dict) or "Error" in s:
                raise ConnectionError(str((s or {}).get("Error") or "the Tuya light did not answer"))
            pct = dev.get_brightness_percentage()
            rgb = None
            if s.get("mode") == "colour":
                got = dev.colour_rgb()
                rgb = [int(c) for c in got] if isinstance(got, (tuple, list)) and len(got) == 3 else None
            return {"on": bool(s.get("is_on")), "brightness": int(pct) if isinstance(pct, (int, float)) else None,
                    "rgb": rgb}
        for name, *args in req["calls"]:
            _tuya_ok(getattr(dev, name)(*args))
        return None
    finally:
        try:
            dev.close()
        except Exception:
            pass


def main():
    req = json.loads(sys.argv[1])
    out = sys.stdout
    try:
        import tinytuya
    except ImportError:
        out.write(json.dumps({"error": {"type": "ImportError", "message": ""}}) + "\n")
        return
    try:
        with contextlib.redirect_stdout(sys.stderr):
            result = run(tinytuya, req)
        out.write(json.dumps({"ok": result}) + "\n")
    except Exception as e:
        out.write(json.dumps({"error": {"type": type(e).__name__, "message": str(e)}}) + "\n")


main()
"#;

fn python() -> String {
    match std::env::var(PYTHON_ENV) {
        Ok(p) if !p.trim().is_empty() => p.trim().to_string(),
        _ => "python3".into(),
    }
}

/// Run the helper once. An interpreter that is missing, or has no tinytuya, is the Python daemon's
/// "tinytuya is not installed".
async fn run_helper(req: &Value) -> Result<Value, LightError> {
    run_helper_with(&python(), req).await
}

pub(crate) async fn run_helper_with(python: &str, req: &Value) -> Result<Value, LightError> {
    let mut cmd = tokio::process::Command::new(python);
    cmd.arg("-c").arg(HELPER).arg(py::dumps(req)).kill_on_drop(true);
    cmd.stdin(std::process::Stdio::null());
    let out = match cmd.output().await {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(LightError::Runtime(NOT_INSTALLED.into())),
        Err(e) => return Err(e.into()),
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
    let reply: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let last = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
            return Err(LightError::Runtime(if last.is_empty() {
                format!("the Tuya helper exited with {}", out.status)
            } else {
                last
            }));
        }
    };
    if let Some(err) = reply.get("error") {
        let kind = err.get("type").and_then(Value::as_str).unwrap_or("Exception").to_string();
        let message = err.get("message").and_then(Value::as_str).unwrap_or("").to_string();
        return Err(match kind.as_str() {
            "ImportError" | "ModuleNotFoundError" => LightError::Runtime(NOT_INSTALLED.into()),
            "ConnectionError" => LightError::Connection(message),
            "TimeoutError" => LightError::Timeout(message),
            "PermissionError" => LightError::Permission(message),
            "OSError" => LightError::Os(message),
            _ => LightError::Python { kind, message },
        });
    }
    Ok(reply.get("ok").cloned().unwrap_or(Value::Null))
}

pub fn tuya_temp_percent(kelvin: i64) -> i64 {
    let (lo, hi) = TUYA_KELVIN;
    py::round_half_even((kelvin.clamp(lo, hi) - lo) as f64 * 100.0 / (hi - lo) as f64) as i64
}

/// The `BulbDevice` calls for a change, in order, each `[method, args...]`.
pub fn tuya_calls(change: &Change) -> Vec<Value> {
    let c = change.normalised();
    if c.power == Some(false) {
        return vec![json!(["turn_off"])];
    }
    let mut out = Vec::new();
    if c.power == Some(true) || !c.is_empty() {
        out.push(json!(["turn_on"]));
    }
    if let Some(k) = c.color.as_ref().and_then(|col| col.kelvin) {
        out.push(json!(["set_white_percentage", c.brightness.unwrap_or(100), tuya_temp_percent(k)]));
        return out;
    }
    if let Some(color) = &c.color {
        let [r, g, b] = color.rgb.unwrap_or([255, 255, 255]);
        out.push(json!(["set_colour", r, g, b]));
    }
    if let Some(b) = c.brightness {
        out.push(json!(["set_brightness_percentage", b]));
    }
    out
}

pub struct TuyaDriver {
    light: Light,
}

impl TuyaDriver {
    pub fn new(light: Light) -> Self {
        TuyaDriver { light }
    }

    fn device(&self) -> Result<Value, LightError> {
        if self.light.key.is_empty() {
            return Err(LightError::Permission("this Tuya light has no local key yet (docs/lights.md, Sylvania)".into()));
        }
        Ok(json!({"id": self.light.id, "ip": self.light.ip, "key": self.light.key, "version": self.light.version}))
    }
}

#[async_trait]
impl Driver for TuyaDriver {
    async fn state(&self) -> Result<State, LightError> {
        let light = self.device()?;
        let got = run_helper(&json!({"op": "state", "light": light})).await?;
        let rgb: Option<Rgb> = got.get("rgb").and_then(Value::as_array).filter(|a| a.len() == 3).map(|a| {
            let ch = |i: usize| a[i].as_i64().unwrap_or(0);
            [ch(0), ch(1), ch(2)]
        });
        Ok(State {
            on: Some(got.get("on").map(py::truthy).unwrap_or(false)),
            brightness: got.get("brightness").and_then(Value::as_i64),
            rgb,
            kelvin: None,
        })
    }

    async fn apply(&self, change: &Change) -> Result<(), LightError> {
        let light = self.device()?;
        run_helper(&json!({"op": "apply", "light": light, "calls": tuya_calls(change)})).await.map(|_| ())
    }

    async fn close(&self) -> Result<(), LightError> {
        // each read or change opens and closes its own connection inside the helper
        Ok(())
    }
}

/// A Tuya device broadcasting on the LAN: `{"id", "ip", "version"}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuyaFound {
    pub id: String,
    pub ip: String,
    pub version: String,
}

/// Tuya devices broadcasting on the LAN. No key needed to find them; none when tinytuya is not installed.
pub async fn tuya_scan(seconds: u64) -> Result<Vec<TuyaFound>, LightError> {
    let got = match run_helper(&json!({"op": "scan", "seconds": seconds})).await {
        Ok(v) => v,
        Err(LightError::Runtime(m)) if m == NOT_INSTALLED => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let s = |d: &Value, k: &str| d.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    Ok(got
        .as_array()
        .map(|items| {
            items.iter().map(|d| TuyaFound { id: s(d, "id"), ip: s(d, "ip"), version: s(d, "version") }).collect()
        })
        .unwrap_or_default())
}
