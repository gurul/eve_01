//! Platform adapter, darwin: the macOS Vision framework through objc2.

use objc2::rc::{autoreleasepool, Retained};
use objc2::AnyThread as _;
use objc2::runtime::{AnyClass, AnyObject};
use objc2_core_foundation::{CFData, CFRetained};
use objc2_core_graphics::{
    CGBitmapInfo, CGColorRenderingIntent, CGColorSpace, CGDataProvider, CGImage, CGImageAlphaInfo,
};
use objc2_foundation::{NSArray, NSDictionary, NSString};
use objc2_image_io::CGImageSource;
use objc2_vision::{VNDetectFaceRectanglesRequest, VNImageOption, VNImageRequestHandler, VNRequest};

use crate::{Detector, Frame, Rect};

/// The Vision-framework detector, or None (with one warning) when unavailable.
pub(crate) fn make_detector() -> Option<Detector> {
    for class in [c"VNDetectFaceRectanglesRequest", c"VNImageRequestHandler"] {
        if AnyClass::get(class).is_none() {
            tracing::warn!(
                "vision: Vision framework class {} not available — board keeps on-board tracking",
                class.to_string_lossy()
            );
            return None;
        }
    }
    Some(std::sync::Arc::new(vision_detect))
}

/// Build a CGImage from the frame bytes. Returns None when decoding fails.
///
/// Gray frames are wrapped as an 8-bit DeviceGray image of the wire's w×h;
/// anything else is decoded as an image file with CGImageSource.
pub fn cgimage_from_frame(frame: &Frame) -> Option<CFRetained<CGImage>> {
    let data = CFData::from_bytes(&frame.data);
    if frame.fmt == "gray" {
        let (w, h) = (usize::try_from(frame.w).ok()?, usize::try_from(frame.h).ok()?);
        let provider = CGDataProvider::with_cf_data(Some(&data))?;
        let cs = CGColorSpace::new_device_gray()?;
        // SAFETY: provider holds w*h bytes for an 8 bpc, 8 bpp, w-byte-row image
        // (decode_frame checked the length; CGImageCreate returns NULL otherwise);
        // a null decode array means the default.
        return unsafe {
            CGImage::new(
                w,
                h,
                8,
                8,
                w,
                Some(&cs),
                CGBitmapInfo(CGImageAlphaInfo::None.0),
                Some(&provider),
                std::ptr::null(),
                false,
                CGColorRenderingIntent::RenderingIntentDefault,
            )
        };
    }
    // SAFETY: data is a valid CFData; no options dictionary.
    let source = unsafe { CGImageSource::with_data(&data, None) }?;
    // SAFETY: index 0 of a valid source; no options dictionary.
    unsafe { source.image_at_index(0, None) }
}

/// Worker thread. VNDetectFaceRectanglesRequest over a CGImage of the frame.
pub(crate) fn vision_detect(frame: &Frame) -> anyhow::Result<Vec<Rect>> {
    autoreleasepool(|_| {
        let Some(image) = cgimage_from_frame(frame) else {
            anyhow::bail!("could not decode {} frame {}", frame.fmt, frame.seq);
        };
        let width = CGImage::width(Some(&image)) as f64;
        let height = CGImage::height(Some(&image)) as f64;
        // SAFETY: plain alloc/init of a Vision request.
        let request = unsafe { VNDetectFaceRectanglesRequest::new() };
        // An empty options dictionary selects Vision's defaults (Python passes nil;
        // a bridged non-empty dict can raise "key does not exist" during option lookup).
        let options = NSDictionary::<VNImageOption, AnyObject>::new();
        // SAFETY: image is a valid CGImage and options is a correctly typed dictionary.
        let handler =
            unsafe { VNImageRequestHandler::initWithCGImage_options(VNImageRequestHandler::alloc(), &image, &options) };
        let as_request: &VNRequest = &request;
        let requests = NSArray::from_slice(&[as_request]);
        if let Err(err) = handler.performRequests_error(&requests) {
            // Python formats the NSError with str(), which is its -description
            // ("Error Domain=… Code=… \"…\" UserInfo={…}").
            // SAFETY: -description on an NSError returns a non-null NSString.
            let desc: Retained<NSString> = unsafe { objc2::msg_send![&*err, description] };
            anyhow::bail!("Vision request failed: {}", desc);
        }
        let mut rects = Vec::new();
        // SAFETY: results of a request that has been performed.
        if let Some(results) = unsafe { request.results() } {
            for obs in results.iter() {
                // SAFETY: plain property reads on a face observation.
                let (bb, conf) = unsafe { (obs.boundingBox(), obs.confidence()) };
                rects.push(Rect {
                    x: bb.origin.x * width,
                    y: (1.0 - bb.origin.y - bb.size.height) * height,
                    w: bb.size.width * width,
                    h: bb.size.height * height,
                    conf: conf as f64,
                });
            }
        }
        Ok(rects)
    })
}

/// (w, h) of an image file's first frame via ImageIO, or None if unreadable.
pub(crate) fn probe_image_size(data: &[u8]) -> Option<(i64, i64)> {
    autoreleasepool(|_| {
        let cf = CFData::from_bytes(data);
        // SAFETY: valid CFData; no options dictionary.
        let source = unsafe { CGImageSource::with_data(&cf, None) }?;
        // SAFETY: index 0 of a valid source; no options dictionary.
        let image = unsafe { source.image_at_index(0, None) }?;
        Some((CGImage::width(Some(&image)) as i64, CGImage::height(Some(&image)) as i64))
    })
}
