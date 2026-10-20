//! The macOS calls the worker makes itself: the Accessibility and Screen Recording checks for this process, and the
//! Quartz events `--release` posts.

use std::ffi::c_void;

use crate::Quartz;

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CGPoint {
    pub x: f64,
    pub y: f64,
}

type CGEventRef = *mut c_void;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> u8;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGEventCreateKeyboardEvent(source: *const c_void, keycode: u16, keydown: bool) -> CGEventRef;
    fn CGEventCreate(source: *const c_void) -> CGEventRef;
    fn CGEventGetLocation(event: CGEventRef) -> CGPoint;
    fn CGEventCreateMouseEvent(source: *const c_void, kind: u32, location: CGPoint, button: u32) -> CGEventRef;
    fn CGEventPost(tap: u32, event: CGEventRef);
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(cf: *const c_void);
}

/// `ApplicationServices.AXIsProcessTrusted()` for this process.
pub fn ax_is_process_trusted() -> bool {
    // SAFETY: no arguments; returns a Boolean.
    unsafe { AXIsProcessTrusted() != 0 }
}

/// `Quartz.CGPreflightScreenCaptureAccess()` for this process.
pub fn screen_capture_access() -> bool {
    // SAFETY: no arguments; returns a bool.
    unsafe { CGPreflightScreenCaptureAccess() }
}

/// One created CGEvent, released when dropped (pyobjc released them for the Python).
pub struct Event(CGEventRef);

impl Drop for Event {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the event came from a CGEventCreate* call (a +1 reference) and is released once.
            unsafe { CFRelease(self.0 as *const c_void) }
        }
    }
}

/// The real Quartz event API.
#[derive(Debug, Default)]
pub struct SystemQuartz;

impl Quartz for SystemQuartz {
    type Event = Event;
    type Point = CGPoint;

    fn create_keyboard_event(&mut self, code: u16, down: bool) -> Event {
        // SAFETY: a NULL source is documented as allowed.
        Event(unsafe { CGEventCreateKeyboardEvent(std::ptr::null(), code, down) })
    }

    fn create(&mut self) -> Event {
        // SAFETY: as above.
        Event(unsafe { CGEventCreate(std::ptr::null()) })
    }

    fn get_location(&mut self, event: &Event) -> CGPoint {
        if event.0.is_null() {
            return CGPoint { x: 0.0, y: 0.0 };
        }
        // SAFETY: a live event.
        unsafe { CGEventGetLocation(event.0) }
    }

    fn create_mouse_event(&mut self, kind: u32, location: CGPoint, button: u32) -> Event {
        // SAFETY: a NULL source is documented as allowed.
        Event(unsafe { CGEventCreateMouseEvent(std::ptr::null(), kind, location, button) })
    }

    fn post(&mut self, tap: u32, event: Event) {
        if !event.0.is_null() {
            // SAFETY: a live event; CGEventPost does not take ownership (Drop releases ours).
            unsafe { CGEventPost(tap, event.0) }
        }
    }
}
