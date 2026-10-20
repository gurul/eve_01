//! `setup_logging`: the daemon's log as full eve's daemon writes it (`LOG_FORMAT`, on stderr: launchd sends it to
//! ~/Library/Logs/eve-lite.log), with the HTTP and WebRTC libraries at WARNING.

use std::fmt;

use tracing::{Event, Level, Subscriber};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::EnvFilter;

/// The Python loggers kept at WARNING (lite.py's QUIET_LOGGERS): their INFO lines name every URL, every address
/// and candidate.
pub const QUIET_LOGGERS: [&str; 4] = ["httpx", "httpcore", "aioice", "aiortc"];

/// Their Rust counterparts: the HTTP client and server stacks (httpx, httpcore) and the WebRTC stack's ICE, DTLS,
/// SCTP, SRTP and mDNS (aioice, aiortc), kept at WARN for the same reason.
pub const QUIET_TARGETS: [&str; 16] = [
    "reqwest",
    "hyper",
    "hyper_util",
    "h2",
    "rustls",
    "tokio_rustls",
    "webrtc",
    "webrtc_ice",
    "webrtc_dtls",
    "webrtc_sctp",
    "webrtc_srtp",
    "webrtc_mdns",
    "webrtc_data",
    "interceptor",
    "turn",
    "stun",
];

/// `logging.getLevelName(str(level or "INFO").strip().upper())`, as a tracing level: a name logging does not know
/// is INFO, not a crash. CRITICAL and FATAL are ERROR here (tracing's highest); NOTSET lets everything through.
pub fn level_of(level: &str) -> Level {
    let named = level.trim().to_uppercase();
    let named = if named.is_empty() { "INFO".to_string() } else { named };
    match named.as_str() {
        "NOTSET" => Level::TRACE,
        "DEBUG" => Level::DEBUG,
        "INFO" => Level::INFO,
        "WARNING" | "WARN" => Level::WARN,
        "ERROR" | "CRITICAL" | "FATAL" => Level::ERROR,
        _ => Level::INFO,
    }
}

/// The filter: `level` for everything, WARN for QUIET_TARGETS.
pub fn log_filter(level: &str) -> EnvFilter {
    let base = level_of(level).to_string().to_lowercase();
    let mut directives = vec![base];
    directives.extend(QUIET_TARGETS.iter().map(|t| format!("{t}=warn")));
    EnvFilter::new(directives.join(","))
}

/// A tracing target as Python's logger name: `eve_lite_web::lite_web` is `eve_bridge.lite_web`, `eve_lite` is
/// `eve_bridge.lite`; anything else keeps its path, dotted.
pub fn logger_name(target: &str) -> String {
    let first = target.split("::").next().unwrap_or(target);
    match first.strip_prefix("eve_") {
        Some(module) if !module.is_empty() => format!("eve_bridge.{module}"),
        _ => target.replace("::", "."),
    }
}

/// Python's level names.
fn level_name(level: &Level) -> &'static str {
    match *level {
        Level::ERROR => "ERROR",
        Level::WARN => "WARNING",
        Level::INFO => "INFO",
        Level::DEBUG | Level::TRACE => "DEBUG",
    }
}

/// `%(asctime)s %(levelname)s %(name)s: %(message)s`; asctime as logging writes it (`2026-10-20 09:19:54,123`).
pub struct PyFormat;

impl<S, N> FormatEvent<S, N> for PyFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(&self, ctx: &FmtContext<'_, S, N>, mut writer: Writer<'_>, event: &Event<'_>) -> fmt::Result {
        let meta = event.metadata();
        let asctime = chrono::Local::now().format("%Y-%m-%d %H:%M:%S,%3f");
        write!(writer, "{asctime} {} {}: ", level_name(meta.level()), logger_name(meta.target()))?;
        ctx.field_format().format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}

/// `logging.basicConfig(level=..., format=LOG_FORMAT)` and QUIET_LOGGERS at WARNING. As basicConfig does nothing
/// once the root logger has a handler, this does nothing once a subscriber is installed.
pub fn setup_logging(level: &str) {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(log_filter(level))
        .with_writer(std::io::stderr)
        .event_format(PyFormat)
        .try_init();
}
