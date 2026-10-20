//! The macOS layer under ax_candidates and desktop_helpers: what pyobjc's `ApplicationServices` and `AppKit`
//! gave the Python (AXUIElement attribute reads, actions, hit-tests; NSRunningApplication / NSWorkspace).
//!
//! Only this module and the `#[cfg(target_os = "macos")]` blocks of desktop_helpers touch objc2. Every read
//! returns what pyobjc's `(err, value)` pair meant: `Err(code)` for a non-zero AXError, else the value already
//! converted the way `ax_candidates.ax_walk.scalar()` and desktop_helpers' readers convert it.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2_app_kit::{NSRunningApplication, NSWorkspace};
use objc2_application_services::{AXCopyMultipleAttributeOptions, AXError, AXUIElement, AXValue, AXValueType};
use objc2_core_foundation::{
    CFArray, CFAttributedString, CFBoolean, CFNumber, CFRetained, CFString, CFType, CGPoint, CGSize, Type,
};

/// kAXErrorFailure: what a call that "succeeded" without handing back a value reads as.
const AX_ERROR_FAILURE: i32 = -25200;

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

/// Take ownership of a CF "Copy" out-parameter.
///
/// # Safety
/// `ptr` is null or a +1 reference of type `T` handed back by a CF Copy/Create call.
unsafe fn owned<T: Type>(ptr: *const T) -> Option<CFRetained<T>> {
    NonNull::new(ptr.cast_mut()).map(|p| unsafe { CFRetained::from_raw(p) })
}

/// pyobjc's bridging of one CF value, then `scalar()`'s reading of it.
fn convert(v: &CFType) -> AxAttr {
    if let Some(b) = v.downcast_ref::<CFBoolean>() {
        return AxAttr::Bool(b.as_bool());
    }
    if let Some(n) = v.downcast_ref::<CFNumber>() {
        // NSNumber bridges to a Python float for a floating type, else to an int.
        return if n.is_float_type() {
            n.as_f64().map_or(AxAttr::Null, AxAttr::Float)
        } else {
            n.as_i64().map_or(AxAttr::Null, AxAttr::Int)
        };
    }
    if let Some(s) = v.downcast_ref::<CFString>() {
        return AxAttr::Str(s.to_string());
    }
    if let Some(value) = v.downcast_ref::<AXValue>() {
        // SAFETY: AXValueGetType reads the tag of a live AXValue.
        let kind = unsafe { value.r#type() };
        if kind == AXValueType::CGPoint {
            let mut pt = CGPoint { x: 0.0, y: 0.0 };
            // SAFETY: the type tag says the payload is a CGPoint, and `pt` is one.
            let ok = unsafe { value.value(AXValueType::CGPoint, NonNull::from(&mut pt).cast::<c_void>()) };
            return if ok { AxAttr::Point(pt.x, pt.y) } else { AxAttr::Null };
        }
        if kind == AXValueType::CGSize {
            let mut sz = CGSize { width: 0.0, height: 0.0 };
            // SAFETY: the type tag says the payload is a CGSize, and `sz` is one.
            let ok = unsafe { value.value(AXValueType::CGSize, NonNull::from(&mut sz).cast::<c_void>()) };
            return if ok { AxAttr::Size(sz.width, sz.height) } else { AxAttr::Null };
        }
        // an AXError value (the attribute is missing), a range or a rect: not useful here
        return AxAttr::Null;
    }
    if let Some(array) = v.downcast_ref::<CFArray>() {
        // SAFETY: a CFArray handed out by the AX API holds CF objects.
        let array: &CFArray<CFType> = unsafe { array.cast_unchecked() };
        return AxAttr::Array(array.iter().map(|item| convert(&item)).collect());
    }
    if let Some(attributed) = v.downcast_ref::<CFAttributedString>() {
        return attributed.string().map_or(AxAttr::Null, |s| AxAttr::Str(s.to_string()));
    }
    if let Some(el) = v.downcast_ref::<AXUIElement>() {
        return AxAttr::Element(AxElement { inner: el.retain() });
    }
    AxAttr::Null
}

impl AxElement {
    /// The raw element (for the few calls this module does not wrap).
    pub fn raw(&self) -> &AXUIElement {
        &self.inner
    }

    /// AXUIElementCreateApplication(pid).
    pub fn application(pid: i32) -> AxElement {
        // SAFETY: creating an application element has no preconditions; a dead pid gives an element whose calls fail.
        AxElement { inner: unsafe { AXUIElement::new_application(pid) } }
    }

    /// AXUIElementCreateSystemWide().
    pub fn system_wide() -> AxElement {
        // SAFETY: no preconditions.
        AxElement { inner: unsafe { AXUIElement::new_system_wide() } }
    }

    /// AXUIElementCopyAttributeValue(self, name): Err(AXError code) when it fails.
    pub fn attribute(&self, name: &str) -> Result<AxAttr, i32> {
        let name = CFString::from_str(name);
        let mut out: *const CFType = std::ptr::null();
        // SAFETY: `out` is a valid out-pointer; on success it holds a +1 reference (or stays null).
        let err = unsafe { self.inner.copy_attribute_value(&name, NonNull::from(&mut out)) };
        // SAFETY: a Copy call's out-parameter is ours to release.
        let value = unsafe { owned(out) };
        if err != AXError::Success {
            return Err(err.0);
        }
        Ok(value.map_or(AxAttr::Null, |v| convert(&v)))
    }

    /// AXUIElementCopyMultipleAttributeValues(self, names, 0): one IPC for every name, in order; an attribute the
    /// element lacks comes back as `AxAttr::Null` (its AXError value). Err(AXError code) when the call fails.
    pub fn attributes(&self, names: &[&str]) -> Result<Vec<AxAttr>, i32> {
        let names: Vec<CFRetained<CFString>> = names.iter().map(|n| CFString::from_str(n)).collect();
        let array = CFArray::from_retained_objects(&names);
        let mut out: *const CFArray = std::ptr::null();
        // SAFETY: `array` holds CFStrings as the call requires; `out` is a valid out-pointer.
        let err = unsafe {
            self.inner.copy_multiple_attribute_values(
                array.as_opaque(),
                AXCopyMultipleAttributeOptions(0),
                NonNull::from(&mut out),
            )
        };
        // SAFETY: a Copy call's out-parameter is ours to release.
        let values = unsafe { owned(out) };
        if err != AXError::Success {
            return Err(err.0);
        }
        let values = values.ok_or(AX_ERROR_FAILURE)?;
        // SAFETY: the values array holds CF objects.
        let values: &CFArray<CFType> = unsafe { values.cast_unchecked() };
        Ok(values.iter().map(|v| convert(&v)).collect())
    }

    /// AXUIElementCopyActionNames(self).
    pub fn action_names(&self) -> Result<Vec<String>, i32> {
        let mut out: *const CFArray = std::ptr::null();
        // SAFETY: `out` is a valid out-pointer.
        let err = unsafe { self.inner.copy_action_names(NonNull::from(&mut out)) };
        // SAFETY: a Copy call's out-parameter is ours to release.
        let names = unsafe { owned(out) };
        if err != AXError::Success {
            return Err(err.0);
        }
        let Some(names) = names else {
            return Ok(Vec::new());
        };
        // SAFETY: the action names array holds CFStrings.
        let names: &CFArray<CFType> = unsafe { names.cast_unchecked() };
        Ok(names
            .iter()
            .map(|n| match convert(&n) {
                AxAttr::Str(s) => s,
                _ => String::new(),
            })
            .collect())
    }

    /// AXUIElementGetPid(self).
    pub fn pid(&self) -> Result<i32, i32> {
        let mut pid: libc::pid_t = 0;
        // SAFETY: `pid` is a valid out-pointer.
        let err = unsafe { self.inner.pid(NonNull::from(&mut pid)) };
        if err != AXError::Success {
            return Err(err.0);
        }
        Ok(pid)
    }

    /// AXUIElementCopyElementAtPosition(self, x, y): the element under a point (points, top-left origin).
    pub fn element_at_position(&self, x: f32, y: f32) -> Result<AxElement, i32> {
        let mut out: *const AXUIElement = std::ptr::null();
        // SAFETY: `out` is a valid out-pointer.
        let err = unsafe { self.inner.copy_element_at_position(x, y, NonNull::from(&mut out)) };
        // SAFETY: a Copy call's out-parameter is ours to release.
        let el = unsafe { owned(out) };
        if err != AXError::Success {
            return Err(err.0);
        }
        el.map(|inner| AxElement { inner }).ok_or(AX_ERROR_FAILURE)
    }

    /// AXUIElementPerformAction(self, action): the AXError code, 0 on success.
    pub fn perform_action(&self, action: &str) -> i32 {
        let action = CFString::from_str(action);
        // SAFETY: a live element and a CFString action name.
        unsafe { self.inner.perform_action(&action) }.0
    }
}

/// NSRunningApplication.runningApplicationWithProcessIdentifier_(pid): (localizedName, bundleIdentifier), each ""
/// when nil; None when no such app.
pub fn running_app(pid: i32) -> Option<(String, String)> {
    objc2::rc::autoreleasepool(|_| {
        let app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)?;
        let name = app.localizedName().map(|s| s.to_string()).unwrap_or_default();
        let bundle = app.bundleIdentifier().map(|s| s.to_string()).unwrap_or_default();
        Some((name, bundle))
    })
}

/// NSWorkspace.sharedWorkspace().runningApplications(): (localizedName or "", processIdentifier) for each.
pub fn running_apps() -> Vec<(String, i32)> {
    objc2::rc::autoreleasepool(|_| {
        let apps = NSWorkspace::sharedWorkspace().runningApplications();
        apps.iter()
            .map(|app| (app.localizedName().map(|s| s.to_string()).unwrap_or_default(), app.processIdentifier()))
            .collect()
    })
}

/// NSWorkspace.sharedWorkspace().frontmostApplication().processIdentifier(), 0 when there is none.
pub fn workspace_frontmost_pid() -> i32 {
    objc2::rc::autoreleasepool(|_| {
        NSWorkspace::sharedWorkspace().frontmostApplication().map_or(0, |app| app.processIdentifier())
    })
}
