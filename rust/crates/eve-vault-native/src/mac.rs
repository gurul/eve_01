//! `NativeMac` on macOS: the ApplicationServices Accessibility API (`AXUIElement*`) and AppKit's
//! `NSRunningApplication`, which is what the Python reaches through pyobjc.

use std::any::Any;
use std::ptr::NonNull;
use std::sync::Arc;

use objc2_app_kit::NSRunningApplication;
use objc2_application_services::{AXError, AXIsProcessTrusted, AXUIElement};
use objc2_core_foundation::{CFArray, CFBoolean, CFNumber, CFRetained, CFString, CFType, CFURL};
use objc2_foundation::NSString;

use crate::{count_of, walk_app, AxTree, AxValue, Element, Field, Native, NativeMac, BROWSERS};

/// A field's AXUIElement. An AXUIElementRef is an immutable CF object: CF's retain and release are thread-safe
/// and the Accessibility API may be called from any thread, so it can cross threads as Python's does.
pub(crate) struct AxElement(CFRetained<AXUIElement>);

// SAFETY: see the type's comment; nothing here mutates the element.
unsafe impl Send for AxElement {}
// SAFETY: as above.
unsafe impl Sync for AxElement {}

/// pyobjc's `==` on two elements is CFEqual. `None` when either is not a Mac element.
pub(crate) fn same_element(a: &(dyn Any + Send + Sync), b: &(dyn Any + Send + Sync)) -> Option<bool> {
    let (a, b) = (a.downcast_ref::<AxElement>()?, b.downcast_ref::<AxElement>()?);
    let a: &CFType = a.0.as_ref();
    let b: &CFType = b.0.as_ref();
    Some(a == b)
}

/// The tree as the Mac has it. A node is any CF object an attribute returned; only an AXUIElement has
/// attributes.
struct MacTree;

fn convert(value: CFRetained<CFType>) -> AxValue<CFRetained<CFType>> {
    if let Some(s) = value.downcast_ref::<CFString>() {
        return AxValue::Str(s.to_string());
    }
    if let Some(b) = value.downcast_ref::<CFBoolean>() {
        return AxValue::Bool(b.as_bool());
    }
    if let Some(n) = value.downcast_ref::<CFNumber>() {
        if n.is_float_type() {
            return AxValue::Float(n.as_f64().unwrap_or(0.0));
        }
        return match n.as_i64() {
            Some(i) => AxValue::Int(i),
            None => AxValue::Other(format!("{:?}", &*value)),
        };
    }
    if let Some(u) = value.downcast_ref::<CFURL>() {
        // NSURL.absoluteString
        let s = match u.absolute_url() {
            Some(abs) => abs.string().to_string(),
            None => u.string().to_string(),
        };
        return AxValue::Url(s);
    }
    if value.downcast_ref::<AXUIElement>().is_some() {
        return AxValue::Element(value);
    }
    if let Some(arr) = value.downcast_ref::<CFArray>() {
        // SAFETY: a CFArray's values are CF objects (the Accessibility API puts nothing else in one).
        let arr: &CFArray<CFType> = unsafe { arr.cast_unchecked::<CFType>() };
        return AxValue::Array(arr.to_vec().into_iter().map(convert).collect());
    }
    AxValue::Other(format!("{:?}", &*value))
}

fn copy_attr(element: &AXUIElement, name: &str) -> Option<CFRetained<CFType>> {
    let attribute = CFString::from_str(name);
    let mut raw: *const CFType = std::ptr::null();
    // SAFETY: `raw` is a valid out pointer; on success it holds a +1 reference (the Copy rule) or NULL.
    let err = unsafe { element.copy_attribute_value(&attribute, NonNull::from(&mut raw)) };
    if err != AXError::Success {
        return None;
    }
    let ptr = NonNull::new(raw as *mut CFType)?;
    // SAFETY: a Copy function's result is owned by the caller.
    Some(unsafe { CFRetained::from_raw(ptr) })
}

fn set_attr(element: &AXUIElement, name: &str, value: &CFType) -> AXError {
    let attribute = CFString::from_str(name);
    // SAFETY: both arguments are valid CF objects for the call's duration.
    unsafe { element.set_attribute_value(&attribute, value) }
}

impl AxTree for MacTree {
    type Node = CFRetained<CFType>;

    fn attr(&self, node: &Self::Node, name: &str) -> Option<AxValue<Self::Node>> {
        let element = node.downcast_ref::<AXUIElement>()?;
        copy_attr(element, name).map(convert)
    }

    fn element(&self, node: &Self::Node) -> Element {
        let element = node.clone().downcast::<AXUIElement>().expect("a walked field is an AXUIElement");
        Arc::new(AxElement(element))
    }
}

fn element_of(field: &Field) -> Option<&AXUIElement> {
    field.element.as_ref()?.downcast_ref::<AxElement>().map(|e| &*e.0)
}

impl Native for NativeMac {
    fn trusted(&self) -> bool {
        // SAFETY: no arguments; reads this process's Accessibility permission.
        unsafe { AXIsProcessTrusted() }
    }

    fn page_fields(&self, want: &dyn Fn(&str) -> bool, frames: bool) -> Vec<Field> {
        let mut out = Vec::new();
        // Asked per bundle id each time: NSWorkspace.runningApplications() is refreshed by the main run loop,
        // which this server does not run, so it kept listing a browser that had quit and never the one that
        // started after (live, 2026-10-12).
        let mut running_apps = Vec::new();
        for bid in BROWSERS {
            let apps = NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(bid));
            for app in apps.iter() {
                if !app.isTerminated() {
                    running_apps.push(app);
                }
            }
        }
        let tree = MacTree;
        for running in running_apps {
            let pid = running.processIdentifier();
            // SAFETY: creating an application element for a pid has no preconditions.
            let app = unsafe { AXUIElement::new_application(pid) };
            let _ = set_attr(&app, "AXManualAccessibility", CFBoolean::new(true).as_ref());
            let name = running.localizedName().map(|s| s.to_string()).unwrap_or_default();
            let app: CFRetained<CFType> = app.into();
            out.extend(walk_app(&tree, pid, &name, &app, want, frames));
        }
        out
    }

    fn set_value(&self, field: &Field, text: &str) -> bool {
        let Some(element) = element_of(field) else { return false };
        let value = CFString::from_str(text);
        set_attr(element, "AXValue", value.as_ref()) == AXError::Success
    }

    fn char_count(&self, field: &Field) -> Option<i64> {
        let element = element_of(field)?;
        count_of(copy_attr(element, "AXNumberOfCharacters").map(convert))
    }
}
