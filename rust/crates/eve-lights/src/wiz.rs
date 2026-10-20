//! WiZ: the local UDP API on port 38899 (`getPilot`, `setPilot`), found by a broadcast `getPilot`. Newer
//! firmware refuses unsigned changes: the home's signing key comes from the WiZ app's local-integration export.

use std::net::{Ipv4Addr, UdpSocket};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine as _;
use hmac::{Hmac, Mac};
use indexmap::IndexMap;
use serde_json::{json, Map, Value};
use sha2::Sha256;
use tracing::info;

use crate::lights::{blocking, Change, Driver, Light, LightError, State};
use crate::py;

pub const WIZ_PORT: u16 = 38899;
/// WiZ colour bulbs; tunable-white ones span 2700-6500 and clamp the rest
pub const WIZ_KELVIN: (i64, i64) = (2200, 6500);
/// the bulb refuses a dimming below this
pub const WIZ_MIN_DIMMING: i64 = 10;

/// The one `setPilot` (or `setState`) params dict for a change. WiZ takes power, colour and level together.
pub fn wiz_params(change: &Change) -> Map<String, Value> {
    let c = change.normalised();
    let mut out = Map::new();
    if c.power == Some(false) {
        out.insert("state".into(), json!(false));
        return out;
    }
    out.insert("state".into(), json!(true));
    if let Some(color) = &c.color {
        match color.kelvin {
            Some(k) => {
                out.insert("temp".into(), json!(k.clamp(WIZ_KELVIN.0, WIZ_KELVIN.1)));
            }
            None => {
                let [r, g, b] = color.rgb.unwrap_or([255, 255, 255]);
                out.insert("r".into(), json!(r));
                out.insert("g".into(), json!(g));
                out.insert("b".into(), json!(b));
            }
        }
    }
    if let Some(b) = c.brightness {
        out.insert("dimming".into(), json!(b.max(WIZ_MIN_DIMMING)));
    }
    out
}

/// A `getPilot` result as a State. A scene (sceneId, no r/g/b or temp) leaves the colour unknown.
pub fn wiz_parse_pilot(result: &Map<String, Value>) -> Result<State, LightError> {
    let k = match result.get("temp") {
        Some(v) if py::truthy(v) => py::int_of(v)?,
        _ => 0,
    };
    let mut rgb = None;
    if k == 0 && ["r", "g", "b"].iter().all(|x| result.get(*x).map(py::is_int).unwrap_or(false)) {
        let ch = |x: &str| py::int_of(&result[x]).unwrap_or(0);
        rgb = Some([ch("r"), ch("g"), ch("b")]);
    }
    let brightness = match result.get("dimming") {
        Some(v) if py::is_number(v) => Some(py::int_of(v)?),
        _ => None,
    };
    Ok(State {
        on: Some(result.get("state").map(py::truthy).unwrap_or(false)),
        brightness,
        rgb,
        kelvin: if k != 0 { Some(k) } else { None },
    })
}

/// The `hmac` a signed WiZ request carries: HMAC-SHA256 of the compact params JSON (`sigTs` included), keyed by
/// the home's hex `udp_signing_key`, base64. Found 2026-09-28 by reproducing the bulb's own reply signatures (it
/// signs its `result` the same way), then confirmed by a signed setPilot it accepted.
pub fn wiz_sign(key_hex: &str, params: &Map<String, Value>) -> Result<String, LightError> {
    let body = py::dumps_compact(&Value::Object(params.clone()));
    let key = py::fromhex(key_hex)?;
    let mut mac = Hmac::<Sha256>::new_from_slice(&key).map_err(|e| LightError::py("ValueError", e.to_string()))?;
    mac.update(body.as_bytes());
    Ok(base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes()))
}

/// A request, signed when there is a key (a read needs none, but a signed one is accepted too).
pub fn wiz_message(
    method: &str,
    params: &Map<String, Value>,
    key_hex: &str,
    sig_ts: Option<i64>,
) -> Result<Vec<u8>, LightError> {
    let mut msg = Map::new();
    msg.insert("id".into(), json!(1));
    msg.insert("method".into(), json!(method));
    if !key_hex.is_empty() && method.starts_with("set") {
        let mut signed = params.clone();
        let ts = sig_ts.unwrap_or_else(|| chrono::Utc::now().timestamp());
        signed.insert("sigTs".into(), json!(ts));
        let hmac = wiz_sign(key_hex, &signed)?;
        msg.insert("params".into(), Value::Object(signed));
        msg.insert("hmac".into(), json!(hmac));
    } else {
        msg.insert("params".into(), Value::Object(params.clone()));
    }
    Ok(py::dumps_compact(&Value::Object(msg)).into_bytes())
}

/// One request and its reply on the WiZ port. UDP drops, so it resends; the reply's `error` becomes an error.
pub fn wiz_call(
    ip: &str,
    method: &str,
    params: &Map<String, Value>,
    tries: u32,
    wait: f64,
    key: &str,
    sig_ts: Option<i64>,
) -> Result<Value, LightError> {
    wiz_call_at(ip, WIZ_PORT, method, params, tries, wait, key, sig_ts)
}

/// [`wiz_call`] on another port (a test's fake bulb).
#[allow(clippy::too_many_arguments)]
pub fn wiz_call_at(
    ip: &str,
    port: u16,
    method: &str,
    params: &Map<String, Value>,
    tries: u32,
    wait: f64,
    key: &str,
    sig_ts: Option<i64>,
) -> Result<Value, LightError> {
    let msg = wiz_message(method, params, key, sig_ts)?;
    let s = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    s.set_read_timeout(Some(Duration::from_secs_f64(wait)))?;
    let mut buf = [0u8; 4096];
    for _ in 0..tries {
        s.send_to(&msg, (ip, port))?;
        let end = Instant::now() + Duration::from_secs_f64(wait);
        while Instant::now() < end {
            let (n, addr) = match s.recv_from(&mut buf) {
                Ok(got) => got,
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => break,
                Err(e) => return Err(e.into()),
            };
            if addr.ip().to_string() != ip {
                continue;
            }
            let Ok(Value::Object(reply)) = serde_json::from_slice::<Value>(&buf[..n]) else { continue };
            if reply.get("method") != Some(&json!(method)) {
                continue;
            }
            if let Some(error) = reply.get("error") {
                let err = error.as_object().cloned().unwrap_or_default();
                let code = err.get("code").and_then(Value::as_f64);
                if code == Some(-32602.0) && method.starts_with("set") {
                    // firmware 1.38 refuses an unsigned change (reads still answer): "Invalid params"
                    return Err(LightError::Refused(if key.is_empty() {
                        "the WiZ light refused the change: it needs the home's signing key \
                         (eve-bridge lights wiz-import, docs/lights.md)"
                            .into()
                    } else {
                        "the WiZ light refused the signed change (a new key? run lights wiz-import again)".into()
                    }));
                }
                let said = match err.get("message") {
                    Some(m) if py::truthy(m) => py::str_of(m),
                    _ => py::str_of(error),
                };
                return Err(LightError::Connection(format!("the WiZ light refused {method}: {said}")));
            }
            return Ok(match reply.get("result") {
                Some(r) if py::truthy(r) => r.clone(),
                _ => Value::Object(Map::new()),
            });
        }
    }
    Err(LightError::Timeout("the WiZ light did not answer; is it on the same Wi-Fi?".into()))
}

/// A change, signed with the Mac's clock; refused once, signed again with the bulb's own clock (its reply to a
/// read carries `sigTs`), in case the two disagree.
pub fn wiz_call_signed(ip: &str, method: &str, params: &Map<String, Value>, key: &str) -> Result<Value, LightError> {
    wiz_call_signed_at(ip, WIZ_PORT, method, params, key)
}

/// [`wiz_call_signed`] on another port.
pub fn wiz_call_signed_at(
    ip: &str,
    port: u16,
    method: &str,
    params: &Map<String, Value>,
    key: &str,
) -> Result<Value, LightError> {
    match wiz_call_at(ip, port, method, params, 3, 0.7, key, None) {
        Err(refused @ LightError::Refused(_)) => {
            if key.is_empty() {
                return Err(refused);
            }
            let pilot = wiz_call_at(ip, port, "getPilot", &Map::new(), 2, 0.7, "", None)?;
            let bulb_ts = match pilot.as_object().and_then(|p| p.get("sigTs")) {
                Some(v) if py::is_int(v) => py::int_of(v)?,
                _ => return Err(refused),
            };
            wiz_call_at(ip, port, method, params, 3, 0.7, key, Some(bulb_ts + 1))
        }
        other => other,
    }
}

/// Take the signing key from the WiZ app's local-integration export (`udp_signing_key`, and `devices` by
/// `mac_address`) into every WiZ light of that home. Returns the names given the key; the error is the
/// Python exception's name, for the CLI's line.
pub fn wiz_import(export: &Value, lights: &mut [Light]) -> Result<Vec<String>, &'static str> {
    let Some(export) = export.as_object() else { return Err("AttributeError") };
    let key = match export.get("udp_signing_key") {
        Some(v) if py::truthy(v) => py::str_of(v),
        _ => String::new(),
    };
    py::fromhex(&key).map_err(|e| e.kind)?; // ValueError when it is not a hex key
    let mut macs: Vec<String> = Vec::new();
    match export.get("devices") {
        Some(Value::Array(devices)) => {
            for d in devices {
                let Some(d) = d.as_object() else { return Err("AttributeError") };
                let mac = match d.get("mac_address") {
                    Some(v) if py::truthy(v) => py::str_of(v).to_lowercase(),
                    _ => String::new(),
                };
                macs.push(mac);
            }
        }
        Some(Value::Object(m)) if !m.is_empty() => return Err("AttributeError"),
        Some(Value::String(s)) if !s.is_empty() => return Err("AttributeError"),
        Some(v) if py::truthy(v) => return Err("TypeError"),
        _ => {}
    }
    let mut done = Vec::new();
    for lt in lights.iter_mut() {
        if lt.kind == "wiz" && (macs.is_empty() || macs.contains(&lt.device.to_lowercase())) {
            lt.key = key.clone();
            done.push(lt.name.clone());
        }
    }
    Ok(done)
}

/// 255.255.255.255 plus this Mac's /24 broadcast: some routers drop the all-ones one.
fn broadcast_addresses() -> Vec<String> {
    let mut out = vec!["255.255.255.255".to_string()];
    let local = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).and_then(|s| {
        s.connect(("10.255.255.255", 1))?; // no packet is sent; it only picks the LAN interface
        s.local_addr()
    });
    if let Ok(std::net::SocketAddr::V4(a)) = local {
        let [a, b, c, _] = a.ip().octets();
        out.push(format!("{a}.{b}.{c}.255"));
    }
    out
}

/// A light the scan found: `{"device" (MAC), "sku" (module), "ip"}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WizFound {
    pub device: String,
    pub sku: String,
    pub ip: String,
}

/// Every WiZ light with local communication on. Blocking.
pub fn wiz_scan(seconds: f64) -> Result<Vec<WizFound>, LightError> {
    let msg = py::dumps(&json!({"method": "getPilot", "params": {}}));
    let mut found: IndexMap<String, WizFound> = IndexMap::new();
    {
        let s = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
        s.set_broadcast(true)?;
        s.set_read_timeout(Some(Duration::from_secs_f64(0.3)))?;
        let end = Instant::now() + Duration::from_secs_f64(seconds);
        let mut again = Instant::now();
        let mut buf = [0u8; 4096];
        while Instant::now() < end {
            if Instant::now() >= again {
                for addr in broadcast_addresses() {
                    let _ = s.send_to(msg.as_bytes(), (addr.as_str(), WIZ_PORT));
                }
                again = Instant::now() + Duration::from_secs(1);
            }
            let (n, addr) = match s.recv_from(&mut buf) {
                Ok(got) => got,
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => continue,
                Err(e) => return Err(e.into()),
            };
            let Ok(reply) = serde_json::from_slice::<Value>(&buf[..n]) else { continue };
            let Some(mac) = reply.get("result").and_then(|r| r.get("mac")) else { continue };
            let mac = py::str_of(mac).to_lowercase();
            found.insert(mac.clone(), WizFound { device: mac, sku: String::new(), ip: addr.ip().to_string() });
        }
    }
    for d in found.values_mut() {
        match wiz_call(&d.ip, "getSystemConfig", &Map::new(), 1, 0.7, "", None) {
            Ok(cfg) => {
                d.sku = match cfg.get("moduleName") {
                    Some(v) if py::truthy(v) => py::str_of(v),
                    _ => String::new(),
                };
            }
            Err(e) if e.is_os() => {}
            Err(e) => return Err(e),
        }
    }
    Ok(found.into_values().collect())
}

pub struct WizDriver {
    light: parking_lot::Mutex<Light>,
    port: u16,
}

impl WizDriver {
    pub fn new(light: Light) -> Self {
        Self::with_port(light, WIZ_PORT)
    }

    pub fn with_port(light: Light, port: u16) -> Self {
        WizDriver { light: parking_lot::Mutex::new(light), port }
    }

    /// The light as the driver knows it now (its address may have moved).
    pub fn light(&self) -> Light {
        self.light.lock().clone()
    }

    /// DHCP moves a light: find it again by its MAC.
    async fn rediscover(&self) -> Result<bool, LightError> {
        if self.light.lock().device.is_empty() {
            return Ok(false);
        }
        for d in blocking(|| wiz_scan(2.0)).await? {
            let mut light = self.light.lock();
            if d.device == light.device.to_lowercase() && d.ip != light.ip {
                info!("lights: {} moved to a new address", light.name);
                light.ip = d.ip;
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn call_once(&self, method: &str, params: &Map<String, Value>) -> Result<Value, LightError> {
        let (ip, key) = {
            let light = self.light.lock();
            (light.ip.clone(), light.key.clone())
        };
        let (port, method, params) = (self.port, method.to_string(), params.clone());
        blocking(move || {
            if method.starts_with("set") {
                wiz_call_signed_at(&ip, port, &method, &params, &key)
            } else {
                wiz_call_at(&ip, port, &method, &params, 3, 0.7, "", None)
            }
        })
        .await
    }

    async fn call(&self, method: &str, params: &Map<String, Value>) -> Result<Value, LightError> {
        if self.light.lock().ip.is_empty() && !self.rediscover().await? {
            return Err(LightError::Connection("no address for this WiZ light; run `eve-bridge lights scan`".into()));
        }
        match self.call_once(method, params).await {
            Err(e @ LightError::Refused(_)) => Err(e),
            Err(e) if e.is_os() => {
                if self.rediscover().await? {
                    self.call_once(method, params).await
                } else {
                    Err(e)
                }
            }
            other => other,
        }
    }
}

#[async_trait]
impl Driver for WizDriver {
    async fn state(&self) -> Result<State, LightError> {
        let result = self.call("getPilot", &Map::new()).await?;
        match result.as_object() {
            Some(m) => wiz_parse_pilot(m),
            None => Err(LightError::py(
                "AttributeError",
                format!("'{}' object has no attribute 'get'", py::type_name(&result)),
            )),
        }
    }

    async fn apply(&self, change: &Change) -> Result<(), LightError> {
        let params = wiz_params(change);
        let off = params.len() == 1 && params.get("state") == Some(&json!(false));
        self.call(if off { "setState" } else { "setPilot" }, &params).await.map(|_| ())
    }

    async fn close(&self) -> Result<(), LightError> {
        // nothing is held open: each request uses its own socket
        Ok(())
    }
}
