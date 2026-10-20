//! The port handle: pyserial's `serial.Serial` (serialposix) for what the
//! transport uses, written on the raw fd so the behavior matches it.
//!
//! - Opened `O_RDWR | O_NOCTTY | O_NONBLOCK` (non-inheritable, as Python's
//!   `os.open` is), locked with `flock(LOCK_EX | LOCK_NB)` for
//!   `exclusive=True` (no `TIOCEXCL`), raw 8N1 without flow control, DTR then
//!   RTS asserted, input flushed.
//! - `read` waits in `poll` on the port and an abort pipe, so `cancel_read`
//!   wakes a blocked reader at once, exactly like pyserial's select loop.
//! - `write` writes, then waits for writability within `write_timeout`, and
//!   fails with [`SerialError::WriteTimeout`] (pyserial's
//!   `SerialTimeoutException('Write timeout')`).
//!
//! Closing while another thread is inside `read` or `write` would close the
//! fd under it (EBADF, or worse, a reused fd). The fds live behind a lock that
//! `read`/`write` hold shared and `close` takes exclusively, so a close waits
//! for the I/O in flight. The transport only closes from the reader, after its
//! read returned, or after a `cancel_read`.

use std::ffi::{CStr, CString};
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use parking_lot::RwLock;

use crate::{SerialError, SerialHandle};

#[cfg(any(target_os = "linux", target_os = "android"))]
mod modem {
    pub const TIOCMBIS: libc::c_ulong = libc::TIOCMBIS as libc::c_ulong;
    pub const TIOCMBIC: libc::c_ulong = libc::TIOCMBIC as libc::c_ulong;
    pub const TIOCM_DTR: libc::c_int = libc::TIOCM_DTR;
    pub const TIOCM_RTS: libc::c_int = libc::TIOCM_RTS;
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
mod modem {
    // <sys/ttycom.h>: _IOW('t', 108, int) and _IOW('t', 107, int).
    pub const TIOCMBIS: libc::c_ulong = 0x8004_746c;
    pub const TIOCMBIC: libc::c_ulong = 0x8004_746b;
    pub const TIOCM_DTR: libc::c_int = 0x002;
    pub const TIOCM_RTS: libc::c_int = 0x004;
}

/// pyserial's `PortNotOpenError` text.
const NOT_OPEN: &str = "Attempting to use a port that is not open";

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// `os.strerror(e)`.
fn strerror(e: i32) -> String {
    // SAFETY: strerror returns a pointer to a static (or thread-local) C string.
    unsafe { CStr::from_ptr(libc::strerror(e)) }.to_string_lossy().into_owned()
}

/// `str(OSError(e, strerror))`: `[Errno 5] Input/output error`.
fn os_error(e: i32) -> String {
    format!("[Errno {e}] {}", strerror(e))
}

fn retryable(e: i32) -> bool {
    e == libc::EAGAIN || e == libc::EALREADY || e == libc::EWOULDBLOCK || e == libc::EINPROGRESS || e == libc::EINTR
}

/// Milliseconds for `poll`, rounded up so a short remainder does not spin.
fn poll_ms(left: Duration) -> libc::c_int {
    let ms = left.as_micros().div_ceil(1000);
    ms.min(libc::c_int::MAX as u128) as libc::c_int
}

fn set_cloexec(fd: RawFd) {
    // SAFETY: plain fcntl on an fd we own.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags >= 0 {
            libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
        }
    }
}

fn set_nonblocking(fd: RawFd) {
    // SAFETY: plain fcntl on an fd we own.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags >= 0 {
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
}

/// `os.pipe()`: both ends non-inheritable; the read end made non-blocking as
/// pyserial does for its abort pipes.
fn abort_pipe() -> Result<(RawFd, RawFd), i32> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: fds has room for the two descriptors pipe writes.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(errno());
    }
    set_cloexec(fds[0]);
    set_cloexec(fds[1]);
    set_nonblocking(fds[0]);
    Ok((fds[0], fds[1]))
}

fn close_fd(fd: RawFd) {
    if fd >= 0 {
        // SAFETY: closing an fd this module opened and still owns.
        unsafe {
            libc::close(fd);
        }
    }
}

struct Fds {
    fd: RawFd,
    abort_read_r: RawFd,
    abort_read_w: RawFd,
    abort_write_r: RawFd,
    abort_write_w: RawFd,
}

impl Drop for Fds {
    fn drop(&mut self) {
        close_fd(self.fd);
        close_fd(self.abort_write_w);
        close_fd(self.abort_write_r);
        close_fd(self.abort_read_w);
        close_fd(self.abort_read_r);
    }
}

/// The speed for `cfsetispeed`/`cfsetospeed`, or `None` for a rate that has no
/// termios constant (pyserial's custom baud path).
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd", target_os = "netbsd", target_os = "openbsd", target_os = "dragonfly"))]
fn speed_constant(baud: u32) -> Option<libc::speed_t> {
    // termios's B* names on macOS; on BSD speed_t is the rate itself.
    const RATES: [u32; 23] = [
        0, 50, 75, 110, 134, 150, 200, 300, 600, 1200, 1800, 2400, 4800, 7200, 9600, 14400, 19200, 28800, 38400,
        57600, 76800, 115200, 230400,
    ];
    RATES.contains(&baud).then_some(baud as libc::speed_t)
}

#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "freebsd", target_os = "netbsd", target_os = "openbsd", target_os = "dragonfly")))]
fn speed_constant(baud: u32) -> Option<libc::speed_t> {
    use libc::*;
    Some(match baud {
        0 => B0,
        50 => B50,
        75 => B75,
        110 => B110,
        134 => B134,
        150 => B150,
        200 => B200,
        300 => B300,
        600 => B600,
        1200 => B1200,
        1800 => B1800,
        2400 => B2400,
        4800 => B4800,
        9600 => B9600,
        19200 => B19200,
        38400 => B38400,
        57600 => B57600,
        115200 => B115200,
        230400 => B230400,
        460800 => B460800,
        500000 => B500000,
        576000 => B576000,
        921600 => B921600,
        1000000 => B1000000,
        1152000 => B1152000,
        1500000 => B1500000,
        2000000 => B2000000,
        2500000 => B2500000,
        3000000 => B3000000,
        3500000 => B3500000,
        4000000 => B4000000,
        _ => return None,
    })
}

/// pyserial's `_set_special_baudrate` on macOS: `IOSSIOSPEED` after tcsetattr.
#[cfg(any(target_os = "macos", target_os = "ios"))]
fn set_special_baudrate(fd: RawFd, baud: u32) -> Result<(), String> {
    // <IOKit/serial/ioss.h>: _IOW('T', 2, speed_t).
    const IOSSIOSPEED: libc::c_ulong = 0x8008_5402;
    let speed: libc::speed_t = baud as libc::speed_t;
    // SAFETY: the ioctl reads one speed_t from the pointer.
    if unsafe { libc::ioctl(fd, IOSSIOSPEED, &speed) } != 0 {
        return Err(os_error(errno()));
    }
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
fn set_special_baudrate(_fd: RawFd, baud: u32) -> Result<(), String> {
    Err(format!("Invalid baud rate: {baud}"))
}

/// pyserial's `_reconfigure_port(force_update=True)` for 8N1, no flow control,
/// `exclusive=True`.
fn configure(fd: RawFd, port: &str, baud: u32) -> Result<(), String> {
    // SAFETY: flock on an fd we own.
    if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let e = errno();
        return Err(format!("[Errno {e}] Could not exclusively lock port {port}: {}", os_error(e)));
    }
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: t is a valid termios for tcgetattr to fill.
    if unsafe { libc::tcgetattr(fd, &mut t) } != 0 {
        let e = errno();
        return Err(format!("Could not configure port: ({e}, '{}')", strerror(e)));
    }
    t.c_cflag |= libc::CLOCAL | libc::CREAD;
    t.c_lflag &= !(libc::ICANON
        | libc::ECHO
        | libc::ECHOE
        | libc::ECHOK
        | libc::ECHONL
        | libc::ISIG
        | libc::IEXTEN
        | libc::ECHOCTL
        | libc::ECHOKE);
    t.c_oflag &= !(libc::OPOST | libc::ONLCR | libc::OCRNL);
    t.c_iflag &= !(libc::INLCR | libc::IGNCR | libc::ICRNL | libc::IGNBRK | libc::PARMRK);
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        t.c_iflag &= !libc::IUCLC;
    }
    let custom = match speed_constant(baud) {
        Some(speed) => {
            // SAFETY: setting speeds on our termios copy.
            unsafe {
                libc::cfsetispeed(&mut t, speed);
                libc::cfsetospeed(&mut t, speed);
            }
            None
        }
        None => {
            // SAFETY: as above.
            unsafe {
                libc::cfsetispeed(&mut t, libc::B38400);
                libc::cfsetospeed(&mut t, libc::B38400);
            }
            Some(baud)
        }
    };
    t.c_cflag &= !libc::CSIZE;
    t.c_cflag |= libc::CS8;
    t.c_cflag &= !libc::CSTOPB;
    t.c_iflag &= !(libc::INPCK | libc::ISTRIP);
    t.c_cflag &= !(libc::PARENB | libc::PARODD);
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        t.c_cflag &= !libc::CMSPAR;
    }
    t.c_iflag &= !(libc::IXON | libc::IXOFF | libc::IXANY);
    t.c_cflag &= !libc::CRTSCTS;
    t.c_cc[libc::VMIN] = 1;
    t.c_cc[libc::VTIME] = 0;
    // SAFETY: t is a fully initialised termios.
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &t) } != 0 {
        let e = errno();
        return Err(format!("({e}, '{}')", strerror(e)));
    }
    if let Some(baud) = custom {
        set_special_baudrate(fd, baud)?;
    }
    Ok(())
}

fn modem_line(fd: RawFd, line: libc::c_int, on: bool) -> Result<(), i32> {
    let req = if on { modem::TIOCMBIS } else { modem::TIOCMBIC };
    let bits: libc::c_int = line;
    // SAFETY: the ioctl reads one int from the pointer.
    if unsafe { libc::ioctl(fd, req as _, &bits) } != 0 {
        return Err(errno());
    }
    Ok(())
}

/// pyserial's open: DTR then RTS raised inside one `try` that ignores
/// "Invalid argument" and "Inappropriate ioctl", so a DTR that fails that way
/// leaves RTS untouched. `Err` carries any other errno.
fn raise_modem_lines(mut raise: impl FnMut(libc::c_int) -> Result<(), i32>) -> Result<(), i32> {
    for line in [modem::TIOCM_DTR, modem::TIOCM_RTS] {
        if let Err(e) = raise(line) {
            if e != libc::EINVAL && e != libc::ENOTTY {
                return Err(e);
            }
            break;
        }
    }
    Ok(())
}

/// One open serial port, the way `serial.Serial(port, baud, timeout=1,
/// write_timeout=2, exclusive=True)` opens it.
pub struct PosixSerial {
    port: String,
    fds: RwLock<Option<Fds>>,
    open: AtomicBool,
    timeout: Duration,
    write_timeout: Duration,
}

impl PosixSerial {
    /// The transport's settings: `timeout=1`, `write_timeout=2`.
    pub fn open(port: &str, baud: u32) -> Result<PosixSerial, SerialError> {
        Self::open_with(port, baud, Duration::from_secs(1), Duration::from_secs(2))
    }

    pub fn open_with(
        port: &str,
        baud: u32,
        timeout: Duration,
        write_timeout: Duration,
    ) -> Result<PosixSerial, SerialError> {
        let cport = CString::new(port).map_err(|_| SerialError::Serial("embedded null byte".to_string()))?;
        // SAFETY: cport is a valid C string.
        let fd = unsafe { libc::open(cport.as_ptr(), libc::O_RDWR | libc::O_NOCTTY | libc::O_NONBLOCK | libc::O_CLOEXEC) };
        if fd < 0 {
            let e = errno();
            return Err(SerialError::Serial(format!(
                "[Errno {e}] could not open port {port}: [Errno {e}] {}: '{port}'",
                strerror(e)
            )));
        }
        let setup = || -> Result<Fds, String> {
            configure(fd, port, baud)?;
            raise_modem_lines(|line| modem_line(fd, line, true)).map_err(os_error)?;
            // SAFETY: tcflush on an fd we own.
            if unsafe { libc::tcflush(fd, libc::TCIFLUSH) } != 0 {
                let e = errno();
                return Err(format!("({e}, '{}')", strerror(e)));
            }
            let (abort_read_r, abort_read_w) = abort_pipe().map_err(os_error)?;
            let (abort_write_r, abort_write_w) = match abort_pipe() {
                Ok(p) => p,
                Err(e) => {
                    close_fd(abort_read_r);
                    close_fd(abort_read_w);
                    return Err(os_error(e));
                }
            };
            Ok(Fds { fd, abort_read_r, abort_read_w, abort_write_r, abort_write_w })
        };
        match setup() {
            Ok(fds) => Ok(PosixSerial {
                port: port.to_string(),
                fds: RwLock::new(Some(fds)),
                open: AtomicBool::new(true),
                timeout,
                write_timeout,
            }),
            Err(msg) => {
                close_fd(fd);
                Err(SerialError::Serial(msg))
            }
        }
    }

    /// pyserial's `cancel_write`: wake a writer waiting for room.
    pub fn cancel_write(&self) {
        if let Some(f) = self.fds.read().as_ref() {
            // SAFETY: one byte into our own pipe.
            unsafe {
                libc::write(f.abort_write_w, b"x".as_ptr().cast(), 1);
            }
        }
    }
}

fn drain(fd: RawFd) {
    let mut sink = [0u8; 1000];
    // SAFETY: reading into a local buffer from our own non-blocking pipe.
    unsafe {
        libc::read(fd, sink.as_mut_ptr().cast(), sink.len());
    }
}

impl SerialHandle for PosixSerial {
    fn read(&self, size: usize) -> Result<Vec<u8>, SerialError> {
        let guard = self.fds.read();
        let f = guard.as_ref().ok_or_else(|| SerialError::Serial(NOT_OPEN.to_string()))?;
        let deadline = Instant::now() + self.timeout;
        let mut out: Vec<u8> = Vec::new();
        while out.len() < size {
            let left = deadline.saturating_duration_since(Instant::now());
            let mut pfds = [
                libc::pollfd { fd: f.fd, events: libc::POLLIN, revents: 0 },
                libc::pollfd { fd: f.abort_read_r, events: libc::POLLIN, revents: 0 },
            ];
            // SAFETY: pfds is a valid array of two pollfd.
            let r = unsafe { libc::poll(pfds.as_mut_ptr(), 2, poll_ms(left)) };
            if r < 0 {
                let e = errno();
                if !retryable(e) {
                    return Err(SerialError::Serial(format!("read failed: {}", os_error(e))));
                }
            } else {
                if pfds[1].revents != 0 {
                    drain(f.abort_read_r);
                    break;
                }
                if r == 0 {
                    break; // timeout
                }
                let mut buf = vec![0u8; size - out.len()];
                // SAFETY: buf has room for the requested bytes.
                let n = unsafe { libc::read(f.fd, buf.as_mut_ptr().cast(), buf.len()) };
                if n < 0 {
                    let e = errno();
                    if !retryable(e) {
                        return Err(SerialError::Serial(format!("read failed: {}", os_error(e))));
                    }
                } else if n == 0 {
                    // Disconnected or other process interfering: select said readable, read gave nothing.
                    return Err(SerialError::Serial(
                        "device reports readiness to read but returned no data \
                         (device disconnected or multiple access on port?)"
                            .to_string(),
                    ));
                } else {
                    out.extend_from_slice(&buf[..n as usize]);
                }
            }
            if Instant::now() >= deadline {
                break;
            }
        }
        Ok(out)
    }

    fn in_waiting(&self) -> Result<usize, SerialError> {
        let guard = self.fds.read();
        let f = guard.as_ref().ok_or_else(|| SerialError::Serial(NOT_OPEN.to_string()))?;
        let mut n: libc::c_int = 0;
        // SAFETY: FIONREAD writes one int.
        if unsafe { libc::ioctl(f.fd, libc::FIONREAD as _, &mut n) } != 0 {
            return Err(SerialError::Serial(os_error(errno())));
        }
        Ok(n.max(0) as usize)
    }

    fn write(&self, data: &[u8]) -> Result<usize, SerialError> {
        let guard = self.fds.read();
        let f = guard.as_ref().ok_or_else(|| SerialError::Serial(NOT_OPEN.to_string()))?;
        let deadline = Instant::now() + self.write_timeout;
        let mut d = data;
        // Wait for room (or an abort) until the deadline; Ok(true) on abort.
        let wait_writable = || -> Result<bool, SerialError> {
            let left = deadline.saturating_duration_since(Instant::now());
            let mut pfds = [
                libc::pollfd { fd: f.abort_write_r, events: libc::POLLIN, revents: 0 },
                libc::pollfd { fd: f.fd, events: libc::POLLOUT, revents: 0 },
            ];
            // SAFETY: pfds is a valid array of two pollfd.
            let r = unsafe { libc::poll(pfds.as_mut_ptr(), 2, poll_ms(left)) };
            if r < 0 {
                let e = errno();
                if retryable(e) {
                    return Ok(false);
                }
                return Err(SerialError::Serial(format!("write failed: {}", os_error(e))));
            }
            if pfds[0].revents != 0 {
                drain(f.abort_write_r);
                return Ok(true);
            }
            if r == 0 {
                return Err(SerialError::WriteTimeout);
            }
            Ok(false)
        };
        while !d.is_empty() {
            // SAFETY: writing from a valid slice.
            let n = unsafe { libc::write(f.fd, d.as_ptr().cast(), d.len()) };
            if n < 0 {
                let e = errno();
                if !retryable(e) {
                    return Err(SerialError::Serial(format!("write failed: {}", os_error(e))));
                }
                if Instant::now() >= deadline {
                    return Err(SerialError::WriteTimeout);
                }
                if wait_writable()? {
                    break;
                }
                continue;
            }
            if Instant::now() >= deadline {
                return Err(SerialError::WriteTimeout);
            }
            let aborted = wait_writable()?;
            d = &d[n as usize..];
            if aborted {
                break;
            }
        }
        Ok(data.len() - d.len())
    }

    fn is_open(&self) -> bool {
        self.open.load(Ordering::SeqCst)
    }

    fn cancel_read(&self) -> Result<(), SerialError> {
        if let Some(f) = self.fds.read().as_ref() {
            // SAFETY: one byte into our own pipe.
            unsafe {
                libc::write(f.abort_read_w, b"x".as_ptr().cast(), 1);
            }
        }
        Ok(())
    }

    fn close(&self) -> Result<(), SerialError> {
        self.open.store(false, Ordering::SeqCst);
        let fds = self.fds.write().take();
        drop(fds);
        Ok(())
    }

    fn set_dtr(&self, on: bool) -> Result<(), SerialError> {
        match self.fds.read().as_ref() {
            Some(f) => modem_line(f.fd, modem::TIOCM_DTR, on).map_err(|e| SerialError::Serial(os_error(e))),
            None => Ok(()), // pyserial stores the state and applies it on the next open
        }
    }

    fn set_rts(&self, on: bool) -> Result<(), SerialError> {
        match self.fds.read().as_ref() {
            Some(f) => modem_line(f.fd, modem::TIOCM_RTS, on).map_err(|e| SerialError::Serial(os_error(e))),
            None => Ok(()),
        }
    }

    fn reset_input_buffer(&self) -> Result<(), SerialError> {
        let guard = self.fds.read();
        let f = guard.as_ref().ok_or_else(|| SerialError::Serial(NOT_OPEN.to_string()))?;
        // SAFETY: tcflush on our fd.
        if unsafe { libc::tcflush(f.fd, libc::TCIFLUSH) } != 0 {
            let e = errno();
            return Err(SerialError::Serial(format!("({e}, '{}')", strerror(e))));
        }
        Ok(())
    }

    fn port(&self) -> Option<String> {
        Some(self.port.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// A pty pair: (master fd, slave path).
    fn pty() -> (RawFd, String) {
        let mut master: libc::c_int = -1;
        let mut slave: libc::c_int = -1;
        let mut name = [0 as libc::c_char; 128];
        // SAFETY: openpty fills the two fds and the name buffer.
        let r = unsafe {
            libc::openpty(&mut master, &mut slave, name.as_mut_ptr(), std::ptr::null_mut(), std::ptr::null_mut())
        };
        assert_eq!(r, 0, "openpty failed");
        let path = unsafe { CStr::from_ptr(name.as_ptr()) }.to_string_lossy().into_owned();
        close_fd(slave);
        (master, path)
    }

    fn open(path: &str, timeout_ms: u64, write_timeout_ms: u64) -> PosixSerial {
        PosixSerial::open_with(
            path,
            115200,
            Duration::from_millis(timeout_ms),
            Duration::from_millis(write_timeout_ms),
        )
        .expect("open pty")
    }

    #[test]
    fn modem_lines_go_up_in_one_try_like_pyserial() {
        let run = |fail: Option<(libc::c_int, i32)>| {
            let mut raised = Vec::new();
            let r = raise_modem_lines(|line| {
                if let Some((bad, e)) = fail {
                    if bad == line {
                        return Err(e);
                    }
                }
                raised.push(line);
                Ok(())
            });
            (r, raised)
        };
        assert_eq!(run(None), (Ok(()), vec![modem::TIOCM_DTR, modem::TIOCM_RTS]));
        // An ignorable DTR failure ends the try: RTS is never raised.
        assert_eq!(run(Some((modem::TIOCM_DTR, libc::ENOTTY))), (Ok(()), vec![]));
        assert_eq!(run(Some((modem::TIOCM_DTR, libc::EINVAL))), (Ok(()), vec![]));
        assert_eq!(run(Some((modem::TIOCM_RTS, libc::EINVAL))), (Ok(()), vec![modem::TIOCM_DTR]));
        assert_eq!(run(Some((modem::TIOCM_DTR, libc::EIO))), (Err(libc::EIO), vec![]));
    }

    #[test]
    fn opening_a_missing_port_reads_like_pyserial() {
        let err = PosixSerial::open("/dev/eve-no-such-port", 115200).err().expect("must fail");
        assert_eq!(
            err.to_string(),
            "[Errno 2] could not open port /dev/eve-no-such-port: \
             [Errno 2] No such file or directory: '/dev/eve-no-such-port'"
        );
    }

    #[test]
    fn a_second_exclusive_open_is_refused() {
        let (master, path) = pty();
        let first = open(&path, 100, 100);
        let err = PosixSerial::open(&path, 115200).err().expect("lock must refuse");
        assert!(err.to_string().contains("Could not exclusively lock port"), "{err}");
        first.close().unwrap();
        assert!(PosixSerial::open(&path, 115200).is_ok(), "the lock goes with the close");
        close_fd(master);
    }

    #[test]
    fn read_returns_what_the_board_sent_and_empty_on_timeout() {
        let (master, path) = pty();
        let ser = open(&path, 100, 500);
        let line = b"{\"a\":1}\n";
        // SAFETY: writing into the pty master.
        unsafe { libc::write(master, line.as_ptr().cast(), line.len()) };
        let first = ser.read(1).unwrap();
        assert_eq!(first, b"{");
        std::thread::sleep(Duration::from_millis(20));
        let waiting = ser.in_waiting().unwrap();
        assert_eq!(waiting, line.len() - 1);
        assert_eq!(ser.read(waiting).unwrap(), &line[1..]);
        let t0 = Instant::now();
        assert!(ser.read(1).unwrap().is_empty());
        assert!(t0.elapsed() >= Duration::from_millis(90));
        close_fd(master);
    }

    #[test]
    fn write_reaches_the_other_end() {
        let (master, path) = pty();
        let ser = open(&path, 100, 500);
        assert_eq!(ser.write(b"{\"time\":1}\n").unwrap(), 11);
        std::thread::sleep(Duration::from_millis(20));
        let mut buf = [0u8; 64];
        // SAFETY: reading from the pty master into a local buffer.
        let n = unsafe { libc::read(master, buf.as_mut_ptr().cast(), buf.len()) };
        assert_eq!(&buf[..n as usize], b"{\"time\":1}\n");
        close_fd(master);
    }

    #[test]
    fn cancel_read_wakes_a_blocked_reader() {
        let (master, path) = pty();
        let ser = Arc::new(open(&path, 5000, 500));
        let reader = {
            let ser = ser.clone();
            std::thread::spawn(move || {
                let t0 = Instant::now();
                let got = ser.read(1).unwrap();
                (got, t0.elapsed())
            })
        };
        std::thread::sleep(Duration::from_millis(50));
        ser.cancel_read().unwrap();
        let (got, took) = reader.join().unwrap();
        assert!(got.is_empty());
        assert!(took < Duration::from_secs(2), "reader stayed blocked {took:?}");
        close_fd(master);
    }

    #[test]
    fn a_board_that_stops_draining_times_the_write_out() {
        let (master, path) = pty();
        let ser = open(&path, 100, 200);
        let chunk = vec![b'x'; 4096];
        let mut outcome = None;
        for _ in 0..1000 {
            match ser.write(&chunk) {
                Ok(_) => continue,
                Err(e) => {
                    outcome = Some(e);
                    break;
                }
            }
        }
        let err = outcome.expect("an undrained pty must fill up");
        assert!(matches!(err, SerialError::WriteTimeout), "{err}");
        assert_eq!(err.to_string(), "Write timeout");
        close_fd(master);
    }

    #[test]
    fn a_closed_port_refuses_io_but_takes_modem_lines() {
        let (master, path) = pty();
        let ser = open(&path, 100, 100);
        assert!(ser.is_open());
        ser.close().unwrap();
        assert!(!ser.is_open());
        assert_eq!(ser.read(1).unwrap_err().to_string(), NOT_OPEN);
        assert_eq!(ser.write(b"x").unwrap_err().to_string(), NOT_OPEN);
        assert_eq!(ser.reset_input_buffer().unwrap_err().to_string(), NOT_OPEN);
        assert!(ser.set_dtr(true).is_ok());
        assert!(ser.cancel_read().is_ok());
        assert_eq!(ser.port().as_deref(), Some(path.as_str()));
        close_fd(master);
    }
}
