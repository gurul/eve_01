//! Platform adapter, darwin: Vision's image feature print through objc2.

use std::sync::Arc;

use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{AnyThread as _, ClassType as _};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::CGImage;
use objc2_foundation::{NSArray, NSDictionary, NSIndexSet, NSString};
use objc2_vision::{
    VNGenerateImageFeaturePrintRequest, VNGenerateImageFeaturePrintRequestRevision2, VNImageOption,
    VNImageRequestHandler, VNRequest,
};

use crate::{crop_rect, Describer, Frame, Rect, Vector};

/// The Vision feature-print describer, or None (with one warning) when unavailable.
pub(crate) fn make_describer() -> Option<Describer> {
    for class in [c"VNGenerateImageFeaturePrintRequest", c"VNImageRequestHandler"] {
        if AnyClass::get(class).is_none() {
            tracing::warn!(
                "identity: Vision framework class {} not available — every face stays 'unknown'",
                class.to_string_lossy()
            );
            return None;
        }
    }
    if !supports_revision2() {
        tracing::warn!("identity: Vision feature print revision 2 needs macOS 14+ — every face stays 'unknown'");
        return None;
    }
    Some(Arc::new(vision_feature_print))
}

/// Python checks `hasattr(Vision, "VNGenerateImageFeaturePrintRequestRevision2")`,
/// i.e. the SDK knows revision 2; here the running framework is asked.
fn supports_revision2() -> bool {
    autoreleasepool(|_| {
        // SAFETY: +supportedRevisions is a VNRequest class method returning a non-null NSIndexSet;
        // the class exists (checked above).
        let revisions: Retained<NSIndexSet> =
            unsafe { objc2::msg_send![VNGenerateImageFeaturePrintRequest::class(), supportedRevisions] };
        revisions.containsIndex(VNGenerateImageFeaturePrintRequestRevision2)
    })
}

/// Worker thread. VNGenerateImageFeaturePrintRequest (revision 2) on the
/// padded face crop.
pub(crate) fn vision_feature_print(frame: &Frame, face: &Rect) -> anyhow::Result<Option<Vector>> {
    autoreleasepool(|_| {
        let Some(image) = eve_vision::cgimage_from_frame(frame) else {
            return Ok(None);
        };
        let width = CGImage::width(Some(&image)) as f64;
        let height = CGImage::height(Some(&image)) as f64;
        let (x, y, w, h) = crop_rect(face, width, height);
        if w < 2 || h < 2 {
            return Ok(None);
        }
        let rect = CGRect::new(CGPoint::new(x as f64, y as f64), CGSize::new(w as f64, h as f64));
        let Some(crop) = CGImage::with_image_in_rect(Some(&image), rect) else {
            return Ok(None);
        };
        // SAFETY: plain alloc/init of a Vision request.
        let request = unsafe { VNGenerateImageFeaturePrintRequest::new() };
        // SAFETY: revision 2 is supported (make_describer checked it).
        unsafe { request.setRevision(VNGenerateImageFeaturePrintRequestRevision2) };
        // Keep options at Vision's defaults: an empty dictionary, as eve-vision's
        // detector does (Python passes nil).
        let options = NSDictionary::<VNImageOption, AnyObject>::new();
        // SAFETY: crop is a valid CGImage and options is a correctly typed dictionary.
        let handler =
            unsafe { VNImageRequestHandler::initWithCGImage_options(VNImageRequestHandler::alloc(), &crop, &options) };
        let as_request: &VNRequest = &request;
        let requests = NSArray::from_slice(&[as_request]);
        if let Err(err) = handler.performRequests_error(&requests) {
            // SAFETY: -description on an NSError returns a non-null NSString.
            let desc: Retained<NSString> = unsafe { objc2::msg_send![&*err, description] };
            anyhow::bail!("Vision feature print failed: {}", desc);
        }
        // SAFETY: results of a request that has been performed.
        let Some(results) = (unsafe { request.results() }) else {
            return Ok(None);
        };
        let Some(obs) = results.iter().next() else {
            return Ok(None);
        };
        // SAFETY: plain property reads on a feature print observation.
        let (count, data) = unsafe { (obs.elementCount(), obs.data()) };
        let data = data.to_vec();
        if data.len() != count * 4 {
            anyhow::bail!("feature print is {} bytes for {} elements; expected float32", data.len(), count);
        }
        let vec = data
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f64)
            .collect();
        Ok(Some(vec))
    })
}
