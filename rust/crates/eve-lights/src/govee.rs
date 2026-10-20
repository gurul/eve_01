//! Govee: the LAN API. JSON over UDP: a scan on multicast 239.255.255.250:4001, replies to :4002, commands to
//! :4003.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Map, Value};
use tracing::info;

use crate::lights::{blocking, Change, Driver, Light, LightError, State};
use crate::py;

pub const GOVEE_GROUP: &str = "239.255.255.250";
pub const GOVEE_SCAN_PORT: u16 = 4001;
pub const GOVEE_LISTEN_PORT: u16 = 4002;
pub const GOVEE_CMD_PORT: u16 = 4003;
pub const GOVEE_KELVIN: (i64, i64) = (2000, 9000);

/// Where the LAN API listens and answers. The defaults are Govee's; a test moves them off the real ports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GoveePorts {
    pub scan: u16,
    pub listen: u16,
    pub cmd: u16,
}

impl Default for GoveePorts {
    fn default() -> Self {
        GoveePorts { scan: GOVEE_SCAN_PORT, listen: GOVEE_LISTEN_PORT, cmd: GOVEE_CMD_PORT }
    }
}

// Every Govee light answers on the one reply port, and with SO_REUSEPORT the kernel hands a unicast reply to
// just one of the sockets bound there: with status("all") reading lights in parallel, one light's reply
// landed in another read's socket and was thrown away as the wrong sender. So one listen at a time.
static GOVEE_LISTEN_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// The reply-port socket, held alone for as long as the caller listens.
fn govee_listening<R>(
    timeout: f64,
    port: u16,
    f: impl FnOnce(&UdpSocket) -> Result<R, LightError>,
) -> Result<R, LightError> {
    let _alone = GOVEE_LISTEN_LOCK.lock();
    let r = govee_listener(timeout, port)?;
    f(&r)
}

fn govee_listener(timeout: f64, port: u16) -> std::io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    s.set_reuse_address(true)?;
    #[cfg(unix)]
    s.set_reuse_port(true)?;
    s.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)).into())?;
    let r: UdpSocket = s.into();
    r.set_read_timeout(Some(Duration::from_secs_f64(timeout)))?;
    Ok(r)
}

fn is_timeout(e: &std::io::Error) -> bool {
    matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)
}

/// A light the scan found: `{"device", "sku", "ip"}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoveeFound {
    pub device: String,
    pub sku: String,
    pub ip: String,
}

/// Every Govee light with LAN Control on. Blocking.
pub fn govee_scan(seconds: f64) -> Result<Vec<GoveeFound>, LightError> {
    govee_scan_on(seconds, GoveePorts::default())
}

pub(crate) fn govee_scan_on(seconds: f64, ports: GoveePorts) -> Result<Vec<GoveeFound>, LightError> {
    let msg = py::dumps(&json!({"msg": {"cmd": "scan", "data": {"account_topic": "reserve"}}}));
    let mut found: IndexMap<String, GoveeFound> = IndexMap::new();
    govee_listening(0.3, ports.listen, |r| {
        let s = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
        s.set_multicast_ttl_v4(2)?;
        let end = Instant::now() + Duration::from_secs_f64(seconds);
        let mut again = Instant::now();
        let mut buf = [0u8; 4096];
        while Instant::now() < end {
            if Instant::now() >= again {
                s.send_to(msg.as_bytes(), (GOVEE_GROUP, ports.scan))?;
                again = Instant::now() + Duration::from_secs(1);
            }
            let (n, addr) = match r.recv_from(&mut buf) {
                Ok(got) => got,
                Err(e) if is_timeout(&e) => continue,
                Err(e) => return Err(e.into()),
            };
            let Ok(reply) = serde_json::from_slice::<Value>(&buf[..n]) else { continue };
            let Some(d) = reply.get("msg").and_then(|m| m.get("data")).and_then(Value::as_object) else {
                continue;
            };
            let device = d.get("device").cloned().unwrap_or(Value::Null);
            if py::truthy(&device) {
                // the light's reply carries its address on most models; the packet's sender always does
                let device = py::str_of(&device);
                let sku = d.get("sku").filter(|v| py::truthy(v)).map(py::str_of).unwrap_or_default();
                let ip = d.get("ip").filter(|v| py::truthy(v)).map(py::str_of).unwrap_or_else(|| addr.ip().to_string());
                found.insert(device.clone(), GoveeFound { device, sku, ip });
            }
        }
        Ok(())
    })?;
    Ok(found.into_values().collect())
}

/// The LAN API messages for a change, in order.
pub fn govee_commands(change: &Change, _current: Option<&State>) -> Vec<Value> {
    let c = change.normalised();
    if c.power == Some(false) {
        return vec![json!({"msg": {"cmd": "turn", "data": {"value": 0}}})];
    }
    let mut out = Vec::new();
    if c.power == Some(true) || !c.is_empty() {
        out.push(json!({"msg": {"cmd": "turn", "data": {"value": 1}}}));
    }
    if let Some(color) = &c.color {
        let data = match color.kelvin {
            Some(k) => {
                let k = k.clamp(GOVEE_KELVIN.0, GOVEE_KELVIN.1);
                json!({"color": {"r": 0, "g": 0, "b": 0}, "colorTemInKelvin": k})
            }
            None => {
                let [r, g, b] = color.rgb.unwrap_or([255, 255, 255]);
                json!({"color": {"r": r, "g": g, "b": b}, "colorTemInKelvin": 0})
            }
        };
        out.push(json!({"msg": {"cmd": "colorwc", "data": data}}));
    }
    if let Some(b) = c.brightness {
        out.push(json!({"msg": {"cmd": "brightness", "data": {"value": b.max(1)}}}));
    }
    out
}

fn obj(v: &Value) -> Result<&Map<String, Value>, LightError> {
    v.as_object().ok_or_else(|| {
        LightError::py("AttributeError", format!("'{}' object has no attribute 'get'", py::type_name(v)))
    })
}

fn key<'a>(m: &'a Map<String, Value>, k: &str) -> Result<&'a Value, LightError> {
    m.get(k).ok_or_else(|| LightError::py("KeyError", py::repr_str(k)))
}

/// One `devStatus` read of the light at `ip`, answered on the reply port. Blocking.
pub(crate) fn govee_status_blocking(ip: &str, ports: GoveePorts) -> Result<State, LightError> {
    govee_listening(1.5, ports.listen, |r| {
        let s = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
        s.send_to(py::dumps(&json!({"msg": {"cmd": "devStatus", "data": {}}})).as_bytes(), (ip, ports.cmd))?;
        let end = Instant::now() + Duration::from_secs_f64(1.5);
        let mut buf = [0u8; 4096];
        while Instant::now() < end {
            let (n, addr) = r.recv_from(&mut buf)?;
            if addr.ip().to_string() != ip {
                continue;
            }
            let reply: Value = serde_json::from_slice(&buf[..n])
                .map_err(|e| LightError::py("JSONDecodeError", e.to_string()))?;
            let d = key(reply.as_object().ok_or_else(|| py::subscript_error(&reply))?, "msg")?;
            if obj(d)?.get("cmd") != Some(&json!("devStatus")) {
                continue;
            }
            let d = obj(key(obj(d)?, "data")?)?;
            let col = match d.get("color") {
                Some(v) if py::truthy(v) => obj(v)?.clone(),
                _ => Map::new(),
            };
            let int_or_0 = |v: Option<&Value>| -> Result<i64, LightError> {
                match v {
                    Some(v) if py::truthy(v) => Ok(py::int_of(v)?),
                    _ => Ok(0),
                }
            };
            let channel = |k: &str| -> Result<i64, LightError> { Ok(py::int_of(col.get(k).unwrap_or(&json!(0)))?) };
            let k = int_or_0(d.get("colorTemInKelvin"))?;
            return Ok(State {
                on: Some(d.get("onOff").map(py::truthy).unwrap_or(false)),
                brightness: Some(int_or_0(d.get("brightness"))?),
                rgb: Some([channel("r")?, channel("g")?, channel("b")?]),
                kelvin: if k != 0 { Some(k) } else { None },
            });
        }
        Err(LightError::Timeout("the Govee light did not answer".into()))
    })
}

pub struct GoveeDriver {
    light: parking_lot::Mutex<Light>,
    ports: GoveePorts,
}

impl GoveeDriver {
    pub fn new(light: Light) -> Self {
        Self::with_ports(light, GoveePorts::default())
    }

    pub fn with_ports(light: Light, ports: GoveePorts) -> Self {
        GoveeDriver { light: parking_lot::Mutex::new(light), ports }
    }

    /// The light as the driver knows it now (its address may have moved).
    pub fn light(&self) -> Light {
        self.light.lock().clone()
    }

    pub(crate) fn status_blocking(&self) -> Result<State, LightError> {
        let ip = self.light.lock().ip.clone();
        govee_status_blocking(&ip, self.ports)
    }

    /// DHCP moves a light: find it again by its device id.
    async fn rediscover(&self) -> Result<bool, LightError> {
        let device = self.light.lock().device.clone();
        if device.is_empty() {
            return Ok(false);
        }
        let ports = self.ports;
        for d in blocking(move || govee_scan_on(2.0, ports)).await? {
            let mut light = self.light.lock();
            if d.device == light.device && d.ip != light.ip {
                info!("lights: {} moved to a new address", light.name);
                light.ip = d.ip;
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn status(&self) -> Result<State, LightError> {
        let ip = self.light.lock().ip.clone();
        let ports = self.ports;
        blocking(move || govee_status_blocking(&ip, ports)).await
    }
}

#[async_trait]
impl Driver for GoveeDriver {
    async fn state(&self) -> Result<State, LightError> {
        match self.status().await {
            Err(e) if e.is_os() => {
                if self.rediscover().await? {
                    self.status().await
                } else {
                    Err(e)
                }
            }
            other => other,
        }
    }

    async fn apply(&self, change: &Change) -> Result<(), LightError> {
        if self.light.lock().ip.is_empty() && !self.rediscover().await? {
            return Err(LightError::Connection("no address for this Govee light; run `eve-bridge lights scan`".into()));
        }
        let msgs: Vec<String> = govee_commands(change, None).iter().map(py::dumps).collect();
        let ip = self.light.lock().ip.clone();
        let port = self.ports.cmd;
        blocking(move || {
            let s = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
            for m in &msgs {
                s.send_to(m.as_bytes(), (ip.as_str(), port))?;
                std::thread::sleep(Duration::from_millis(50)); // the lamp drops a burst; a gap keeps every message
            }
            Ok(())
        })
        .await
    }

    async fn close(&self) -> Result<(), LightError> {
        // nothing is held open: each read and change uses its own socket
        Ok(())
    }
}
