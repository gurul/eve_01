//! The real [`Ax`] on macOS: ApplicationServices' `AXUIElement*` calls, which the Python reaches through
//! pyobjc. Everything here runs on one blocking thread per call; no element crosses threads.

use std::ptr::NonNull;

use objc2_application_services::{AXError, AXUIElement};
use objc2_core_foundation::{CFArray, CFBoolean, CFNumber, CFRetained, CFString, CFType};

use crate::{Ax, AxValue};

pub(crate) struct MacAx;

fn convert(value: CFRetained<CFType>) -> AxValue<CFRetained<AXUIElement>> {
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
    if value.downcast_ref::<AXUIElement>().is_some() {
        return match value.downcast::<AXUIElement>() {
            Ok(el) => AxValue::Element(el),
            Err(value) => AxValue::Other(format!("{:?}", &*value)),
        };
    }
    if let Some(arr) = value.downcast_ref::<CFArray>() {
        // SAFETY: a CFArray's values are CF objects (the Accessibility API puts nothing else in one).
        let arr: &CFArray<CFType> = unsafe { arr.cast_unchecked::<CFType>() };
        return AxValue::Array(arr.to_vec().into_iter().map(convert).collect());
    }
    AxValue::Other(format!("{:?}", &*value))
}

impl Ax for MacAx {
    type Node = CFRetained<AXUIElement>;

    fn application(&self, pid: i32) -> Self::Node {
        // SAFETY: creating an application element for a pid has no preconditions.
        unsafe { AXUIElement::new_application(pid) }
    }

    fn attr(&self, el: &Self::Node, name: &str) -> Option<AxValue<Self::Node>> {
        let attribute = CFString::from_str(name);
        let mut raw: *const CFType = std::ptr::null();
        // SAFETY: `raw` is a valid out pointer; on success it holds a +1 reference (the Copy rule) or NULL.
        let err = unsafe { el.copy_attribute_value(&attribute, NonNull::from(&mut raw)) };
        if err != AXError::Success {
            return None;
        }
        let ptr = NonNull::new(raw as *mut CFType)?;
        // SAFETY: a Copy function's result is owned by the caller.
        Some(convert(unsafe { CFRetained::from_raw(ptr) }))
    }

    fn perform(&self, el: &Self::Node, action: &str) -> i32 {
        let action = CFString::from_str(action);
        // SAFETY: both arguments are valid CF objects for the call's duration.
        unsafe { el.perform_action(&action) }.0
    }
}
