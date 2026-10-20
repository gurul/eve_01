//! Platform adapter, darwin: the ImageIO + Core Graphics thumbnailer behind `quartz_thumb`.

use objc2::rc::autoreleasepool;
use objc2_core_foundation::{CFData, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextCreateImage, CGColorSpace, CGContext, CGDataProvider, CGImage,
    CGImageAlphaInfo, CGInterpolationQuality,
};
use objc2_image_io::CGImageSource;

/// Decode any ImageIO-readable file and draw it into a tw x th 8-bit DeviceGray
/// bitmap at low interpolation quality; the rows come back packed (stride dropped).
pub(crate) fn quartz_thumb(image: &[u8], tw: usize, th: usize) -> Option<Vec<u8>> {
    autoreleasepool(|_| {
        let data = CFData::from_bytes(image);
        // SAFETY: data is a valid CFData; no options dictionary.
        let source = unsafe { CGImageSource::with_data(&data, None) }?;
        // SAFETY: index 0 of a valid source; no options dictionary.
        let cgimage = unsafe { source.image_at_index(0, None) }?;
        let space = CGColorSpace::new_device_gray()?;
        // SAFETY: a null data pointer asks Core Graphics to allocate the bitmap;
        // bytes_per_row 0 lets it pick the stride.
        let ctx = unsafe {
            CGBitmapContextCreate(std::ptr::null_mut(), tw, th, 8, 0, Some(&space), CGImageAlphaInfo::None.0)
        }?;
        CGContext::set_interpolation_quality(Some(&ctx), CGInterpolationQuality::Low);
        let rect = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(tw as f64, th as f64));
        CGContext::draw_image(Some(&ctx), rect, Some(&cgimage));
        let thumb = CGBitmapContextCreateImage(Some(&ctx))?;
        let stride = CGImage::bytes_per_row(Some(&thumb));
        let provider = CGImage::data_provider(Some(&thumb))?;
        let raw = CGDataProvider::data(Some(&provider))?.to_vec();
        // Python slices raw[y*stride : y*stride+tw], which clamps at the end of the buffer.
        let mut out = Vec::with_capacity(tw * th);
        for y in 0..th {
            let start = (y * stride).min(raw.len());
            let end = (y * stride + tw).min(raw.len());
            out.extend_from_slice(&raw[start..end]);
        }
        Some(out)
    })
}
