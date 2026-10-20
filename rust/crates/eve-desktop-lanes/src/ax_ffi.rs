//! The macOS layer under ax_candidates and desktop_helpers: what pyobjc's `ApplicationServices` and `AppKit`
//! gave the Python (AXUIElement attribute reads, actions, hit-tests; NSRunningApplication / NSWorkspace).
//!
//! Only this module and the `#[cfg(target_os = "macos")]` blocks of desktop_helpers touch objc2. Every read
//! returns what pyobjc's `(err, value)` pair meant: `Err(code)` for a non-zero AXError, else the value already
//! converted the way `ax_candidates.ax_walk.scalar()` and desktop_helpers' readers convert it.

use objc2_application_services::AXUIElement;
use objc2_core_foundation::CFRetained;

/// One live AXUIElement, valid only in this process and only while the app keeps the element.
#[derive(Clone)]
pub struct AxElement {
    inner: CFRetained<AXUIElement>,
}

// SAFETY: AXUIElement is a CF type whose calls are IPC to the owning app; Apple documents the AX API as callable
// from any thread, and the reference count is atomic.
unsafe impl Send for AxElement {}
unsafe impl Sync for AxElement {}

impl std::fmt::Debug for AxElement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AxElement").finish_non_exhaustive()
    }
}

/// An attribute value as the Python readers saw it after `scalar()`: an AXError value, a range, a rect and any
/// other non-point/size AXValue read as `Null`; an NSAttributedString as its plain string.
#[derive(Debug, Clone)]
pub enum AxAttr {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    /// kAXValueCGPointType: (x, y)
    Point(f64, f64),
    /// kAXValueCGSizeType: (width, height)
    Size(f64, f64),
    Array(Vec<AxAttr>),
    Element(AxElement),
}

impl AxElement {
    /// The raw element (for the few calls this module does not wrap).
    pub fn raw(&self) -> &AXUIElement {
        &self.inner
    }

    /// AXUIElementCreateApplication(pid).
    pub fn application(pid: i32) -> AxElement {
        todo!("ax_ffi: AXUIElementCreateApplication")
    }

    /// AXUIElementCreateSystemWide().
    pub fn system_wide() -> AxElement {
        todo!("ax_ffi: AXUIElementCreateSystemWide")
    }

    /// AXUIElementCopyAttributeValue(self, name): Err(AXError code) when it fails.
    pub fn attribute(&self, name: &str) -> Result<AxAttr, i32> {
        todo!("ax_ffi: AXUIElementCopyAttributeValue")
    }

    /// AXUIElementCopyMultipleAttributeValues(self, names, 0): one IPC for every name, in order; an attribute the
    /// element lacks comes back as `AxAttr::Null` (its AXError value). Err(AXError code) when the call fails.
    pub fn attributes(&self, names: &[&str]) -> Result<Vec<AxAttr>, i32> {
        todo!("ax_ffi: AXUIElementCopyMultipleAttributeValues")
    }

    /// AXUIElementCopyActionNames(self).
    pub fn action_names(&self) -> Result<Vec<String>, i32> {
        todo!("ax_ffi: AXUIElementCopyActionNames")
    }

    /// AXUIElementGetPid(self).
    pub fn pid(&self) -> Result<i32, i32> {
        todo!("ax_ffi: AXUIElementGetPid")
    }

    /// AXUIElementCopyElementAtPosition(self, x, y): the element under a point (points, top-left origin).
    pub fn element_at_position(&self, x: f32, y: f32) -> Result<AxElement, i32> {
        todo!("ax_ffi: AXUIElementCopyElementAtPosition")
    }

    /// AXUIElementPerformAction(self, action): the AXError code, 0 on success.
    pub fn perform_action(&self, action: &str) -> i32 {
        todo!("ax_ffi: AXUIElementPerformAction")
    }
}

/// NSRunningApplication.runningApplicationWithProcessIdentifier_(pid): (localizedName, bundleIdentifier), each ""
/// when nil; None when no such app.
pub fn running_app(pid: i32) -> Option<(String, String)> {
    todo!("ax_ffi: NSRunningApplication by pid")
}

/// NSWorkspace.sharedWorkspace().runningApplications(): (localizedName or "", processIdentifier) for each.
pub fn running_apps() -> Vec<(String, i32)> {
    todo!("ax_ffi: NSWorkspace runningApplications")
}

/// NSWorkspace.sharedWorkspace().frontmostApplication().processIdentifier(), 0 when there is none.
pub fn workspace_frontmost_pid() -> i32 {
    todo!("ax_ffi: NSWorkspace frontmostApplication")
}
