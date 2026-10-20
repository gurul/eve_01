//! What Pillow's `Image.open` would say about an image, and whether its pixels
//! decode.
//!
//! `validate` gives one of three messages, and which one depends on where
//! Pillow fails: a header it cannot parse (or a decompression bomb) is
//! "couldn't read" even when the image is also animated or huge; an animated
//! or non-still-format image is "send a still" even when its pixel data is
//! broken. So [`open`] follows Pillow 12's JPEG, PNG, GIF and WebP header
//! parsers (`JpegImagePlugin._open` and `jpeg_factory`, `PngImageFile._open`,
//! `GifImageFile._open` and `n_frames`, `WebPImageFile._open`), and
//! [`verify_and_load`] does what `verify()` and `load()` check afterwards.

use std::io::Cursor;

/// One of `FORMATS`, by index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    Jpeg = 0,
    Png = 1,
    Webp = 2,
    Gif = 3,
}

/// Pillow could not open or decode the image. The reason is not shown.
#[derive(Debug)]
pub(crate) struct Broken;

type Probe<T> = Result<T, Broken>;

#[derive(Debug)]
pub(crate) struct Opened {
    /// `None` for an image Pillow opens as a format outside `FORMATS` (BMP,
    /// TIFF, a multi-picture JPEG, …).
    pub format: Option<Format>,
    pub width: u32,
    pub height: u32,
    pub n_frames: u64,
    /// JPEG: where the entropy-coded data of the first scan starts.
    /// PNG: where the first image-data chunk's body starts.
    data_at: Option<usize>,
}

/// `Image.MAX_IMAGE_PIXELS`; `Image.open` refuses more than twice this.
const MAX_IMAGE_PIXELS: u64 = 1024 * 1024 * 1024 / 4 / 3;
const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

/// `Image._decompression_bomb_check`.
fn bomb_check(width: u64, height: u64) -> Probe<()> {
    if width.max(1) * height.max(1) > 2 * MAX_IMAGE_PIXELS {
        return Err(Broken);
    }
    Ok(())
}

/// A file read the way Pillow reads it: short reads at the end, no panics.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8], pos: usize) -> Self {
        Self { data, pos: pos.min(data.len()) }
    }

    /// `fp.read(1)`.
    fn byte(&mut self) -> Option<u8> {
        let b = self.data.get(self.pos).copied();
        if b.is_some() {
            self.pos += 1;
        }
        b
    }

    /// `fp.read(n)`: up to `n` bytes.
    fn read(&mut self, n: usize) -> &'a [u8] {
        let end = self.pos.saturating_add(n).min(self.data.len());
        let out = &self.data[self.pos..end];
        self.pos = end;
        out
    }

    /// `ImageFile._safe_read(fp, n)`: exactly `n` bytes, empty for `n <= 0`.
    fn safe_read(&mut self, n: i64) -> Probe<&'a [u8]> {
        if n <= 0 {
            return Ok(&[]);
        }
        let n = usize::try_from(n).map_err(|_| Broken)?;
        let out = self.read(n);
        if out.len() < n {
            return Err(Broken);
        }
        Ok(out)
    }
}

fn be16(s: &[u8], at: usize) -> Probe<u16> {
    s.get(at..at + 2).map(|b| u16::from_be_bytes([b[0], b[1]])).ok_or(Broken)
}

fn be32(s: &[u8], at: usize) -> Probe<u32> {
    s.get(at..at + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]])).ok_or(Broken)
}

fn le16(s: &[u8], at: usize) -> Probe<u16> {
    s.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]])).ok_or(Broken)
}

/// `Image.open`: identify the format and read the header.
pub(crate) fn open(data: &[u8]) -> Probe<Opened> {
    let opened = if data.starts_with(b"\xff\xd8\xff") {
        open_jpeg(data)?
    } else if data.starts_with(PNG_MAGIC) {
        open_png(data)?
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        open_gif(data)?
    } else if data.starts_with(b"RIFF")
        && data.get(8..12) == Some(b"WEBP")
        && matches!(data.get(12..16), Some(b"VP8 " | b"VP8X" | b"VP8L"))
    {
        open_webp(data)?
    } else {
        open_other(data)?
    };
    bomb_check(opened.width.into(), opened.height.into())?;
    Ok(opened)
}

/// Pillow's `verify()` then `load()` on a fresh open.
pub(crate) fn verify_and_load(opened: &Opened, data: &[u8]) -> Probe<()> {
    let format = match opened.format.ok_or(Broken)? {
        Format::Jpeg => {
            // Pillow stops loading with "image file is truncated" when the
            // decoder asks for data past the end, which is what a JPEG
            // without its end-of-image marker does.
            let scan = data.get(opened.data_at.ok_or(Broken)?..).ok_or(Broken)?;
            if !scan.windows(2).any(|w| w == [0xFF, 0xD9]) {
                return Err(Broken);
            }
            image::ImageFormat::Jpeg
        }
        Format::Png => {
            verify_png(data, opened.data_at.ok_or(Broken)?)?;
            image::ImageFormat::Png
        }
        Format::Gif => image::ImageFormat::Gif,
        Format::Webp => return load_webp(data),
    };
    image::load_from_memory_with_format(data, format).map(drop).map_err(|_| Broken)
}

/// An image the `image` crate recognises in a format outside `FORMATS`: Pillow
/// opens it (so the owner hears "send a still") when its header parses.
fn open_other(data: &[u8]) -> Probe<Opened> {
    use image::ImageFormat as F;
    let format = image::guess_format(data).map_err(|_| Broken)?;
    if matches!(format, F::Jpeg | F::Png | F::Gif | F::WebP) {
        return Err(Broken);
    }
    let (width, height) = image::ImageReader::with_format(Cursor::new(data), format)
        .into_dimensions()
        .map_err(|_| Broken)?;
    Ok(Opened { format: None, width, height, n_frames: 1, data_at: None })
}

// ---------------------------------------------------------------- JPEG

/// `JpegImageFile._open`, then `jpeg_factory`'s multi-picture check.
fn open_jpeg(data: &[u8]) -> Probe<Opened> {
    let mut r = Reader::new(data, 3);
    let mut s: Option<u8> = Some(0xFF);
    let (mut width, mut height) = (0u32, 0u32);
    let mut mp: Option<&[u8]> = None;
    let mut app1: Vec<&[u8]> = Vec::new();
    let mut icc: Vec<&[u8]> = Vec::new();
    loop {
        let i = s.ok_or(Broken)?;
        if i != 0xFF {
            // Skip non-0xFF junk
            s = r.byte();
            continue;
        }
        let marker = 0xFF00 | u16::from(r.byte().ok_or(Broken)?);
        match marker {
            0xFFC0..=0xFFFE => {
                match marker {
                    0xFFC8 | 0xFFD0..=0xFFD9 | 0xFFF0..=0xFFFD => {}
                    0xFFC4 | 0xFFCC | 0xFFDA | 0xFFDC | 0xFFDD | 0xFFDF => {
                        segment(&mut r)?;
                    }
                    0xFFDB => dqt(segment(&mut r)?)?,
                    0xFFE0..=0xFFEF => {
                        let s = segment(&mut r)?;
                        app(marker, s, &mut mp, &mut app1, &mut icc)?;
                    }
                    0xFFFE => {
                        segment(&mut r)?;
                    }
                    _ => {
                        // SOF0-3, 5-7, 9-11, 13-15 and DHP.
                        let s = segment(&mut r)?;
                        width = be16(s, 3)?.into();
                        height = be16(s, 1)?.into();
                        if s[0] != 8 {
                            return Err(Broken); // cannot handle non-8-bit layers
                        }
                        if !matches!(s.get(5).ok_or(Broken)?, 1 | 3 | 4) {
                            return Err(Broken);
                        }
                        if !icc.is_empty() {
                            icc.sort();
                            if icc[0].len() < 14 {
                                return Err(Broken);
                            }
                            icc.clear();
                        }
                    }
                }
                if marker == 0xFFDA {
                    break;
                }
                s = r.byte();
            }
            // padded marker or junk; move on
            0xFFFF => s = Some(0xFF),
            // Skip extraneous data (escaped 0xFF)
            0xFF00 => s = r.byte(),
            _ => return Err(Broken), // no marker found
        }
    }
    let multi_picture = match mp {
        None => false,
        Some(mp) => match multi_picture_count(mp)? {
            Some(count) if count > 1 => {
                // Ultra HDR images are not yet supported (Pillow opens them as plain JPEG).
                !app1.iter().any(|s| contains(s, b" hdrgm:Version=\""))
            }
            _ => false,
        },
    };
    Ok(Opened {
        format: if multi_picture { None } else { Some(Format::Jpeg) },
        width,
        height,
        n_frames: 1,
        data_at: Some(r.pos),
    })
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// A marker segment: `n = i16(fp.read(2)) - 2`, then `_safe_read(fp, n)`.
fn segment<'a>(r: &mut Reader<'a>) -> Probe<&'a [u8]> {
    let n = be16(r.read(2), 0)?;
    r.safe_read(i64::from(n) - 2)
}

/// `DQT`: every table in the segment must be whole.
fn dqt(mut s: &[u8]) -> Probe<()> {
    while let Some(&v) = s.first() {
        let precision = if v / 16 == 0 { 1 } else { 2 };
        let qt_length = 1 + precision * 64;
        if s.len() < qt_length {
            return Err(Broken); // bad quantization table marker
        }
        s = &s[qt_length..];
    }
    Ok(())
}

/// `APP`: the parts of the application-segment handler that can fail, plus the
/// segments `jpeg_factory` reads afterwards. (Pillow's other branches, Exif,
/// XMP and FlashPix, only store the segment.)
fn app<'a>(
    marker: u16,
    s: &'a [u8],
    mp: &mut Option<&'a [u8]>,
    app1: &mut Vec<&'a [u8]>,
    icc: &mut Vec<&'a [u8]>,
) -> Probe<()> {
    if marker == 0xFFE1 {
        app1.push(s);
    }
    if marker == 0xFFE0 && s.starts_with(b"JFIF") {
        be16(s, 5)?;
    } else if marker == 0xFFE2 && s.starts_with(b"ICC_PROFILE\0") {
        icc.push(s);
    } else if marker == 0xFFED && s.starts_with(b"Photoshop 3.0\x00") {
        photoshop(s)?;
    } else if marker == 0xFFEE && s.starts_with(b"Adobe") {
        be16(s, 5)?;
    } else if marker == 0xFFE2 && s.starts_with(b"MPF\0") {
        *mp = Some(&s[4..]);
    }
    Ok(())
}

/// The Photoshop resource walk: running out of data ends it quietly, except a
/// resource name length past the end, which Pillow does not guard.
fn photoshop(s: &[u8]) -> Probe<()> {
    let mut offset = 14usize;
    while s.get(offset..offset.saturating_add(4)) == Some(b"8BIM") {
        offset += 4;
        let Ok(code) = be16(s, offset) else {
            return Ok(());
        };
        offset += 2;
        let name_len = usize::from(*s.get(offset).ok_or(Broken)?);
        offset += 1 + name_len;
        offset += offset & 1;
        let Ok(size) = be32(s, offset) else {
            return Ok(());
        };
        offset += 4;
        let size = size as usize;
        if code == 0x03ED {
            // ResolutionInfo: four fields over 14 bytes; short data ends the walk.
            let block = &s[offset.min(s.len())..offset.saturating_add(size).min(s.len())];
            if block.len() < 14 {
                return Ok(());
            }
        }
        offset = offset.saturating_add(size);
        offset += offset & 1;
    }
    Ok(())
}

/// `_getmp`: the number of pictures in a JPEG's multi-picture index.
///
/// `Ok(None)` where Pillow gives up on the index and keeps the file as a plain
/// JPEG (an unreadable directory, a missing count or entry list, a value of
/// the wrong type, a non-JPEG entry). `Err` where the entry list is shorter
/// than the count says, which Pillow's open does not survive.
fn multi_picture_count(mp: &[u8]) -> Probe<Option<i64>> {
    let head = &mp[..mp.len().min(8)];
    let entries_big_endian = head.starts_with(b"MM\x00\x2a");
    const PREFIXES: [&[u8]; 6] =
        [b"MM\x00\x2a", b"II\x2a\x00", b"MM\x2a\x00", b"II\x00\x2a", b"MM\x00\x2b", b"II\x2b\x00"];
    if head.len() < 4 || !PREFIXES.contains(&&head[..4]) {
        return Ok(None);
    }
    let big_endian = &head[..2] == b"MM";
    if head[2] == 43 || head.len() < 8 {
        return Ok(None); // BigTIFF needs a 16-byte header; this one is 8
    }
    let u32_at = |s: &[u8], at: usize, big: bool| -> Option<u32> {
        let b = s.get(at..at + 4)?;
        let b = [b[0], b[1], b[2], b[3]];
        Some(if big { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) })
    };
    let next = u32_at(head, 4, big_endian).ok_or(Broken)? as usize;
    let tags = ifd_tags(mp, next, big_endian);
    let Some((count_type, count_data)) = tags.iter().rev().find(|t| t.0 == 0xB001).map(|t| (t.1, t.2)) else {
        return Ok(None);
    };
    let Some(quant) = first_int(count_type, count_data, big_endian) else {
        return Ok(None); // range() of a non-int
    };
    let Some((entry_type, raw)) = tags.iter().rev().find(|t| t.0 == 0xB002).map(|t| (t.1, t.2)) else {
        return Ok(None);
    };
    if quant >= 1 && !matches!(entry_type, 1 | 7) {
        return Ok(None); // struct.unpack_from of a non-buffer
    }
    for entry in 0..quant.max(0) as u64 {
        let at = usize::try_from(entry * 16).map_err(|_| Broken)?;
        if at.checked_add(16).is_none_or(|end| end > raw.len()) {
            return Err(Broken);
        }
        let attribute = u32_at(raw, at, entries_big_endian).ok_or(Broken)?;
        if (attribute >> 24) & 7 != 0 {
            return Ok(None); // unsupported picture format in MPO
        }
    }
    Ok(Some(quant))
}

/// `TiffImagePlugin` loader unit sizes by field type.
fn unit_size(typ: u16) -> Option<u64> {
    Some(match typ {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 | 13 => 4,
        5 | 10 | 12 | 16 => 8,
        _ => return None,
    })
}

/// `ImageFileDirectory_v2.load`: the raw value of each tag, in file order.
/// A read past the end stops the walk and keeps the tags read so far.
fn ifd_tags(buf: &[u8], offset: usize, big: bool) -> Vec<(u16, u16, &[u8])> {
    let u16_at = |at: usize| -> Option<u16> {
        let b = buf.get(at..at + 2)?;
        Some(if big { u16::from_be_bytes([b[0], b[1]]) } else { u16::from_le_bytes([b[0], b[1]]) })
    };
    let u32_of = |b: &[u8]| -> u32 {
        let b = [b[0], b[1], b[2], b[3]];
        if big { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) }
    };
    let mut tags = Vec::new();
    let Some(count) = u16_at(offset) else { return tags };
    let mut pos = offset + 2;
    for _ in 0..count {
        let Some(entry) = buf.get(pos..pos + 12) else { return tags };
        pos += 12;
        let tag = u16_at(pos - 12).unwrap_or_default();
        let typ = u16_at(pos - 10).unwrap_or_default();
        let count = u64::from(u32_of(&entry[4..8]));
        let Some(unit) = unit_size(typ) else { continue };
        let size = count * unit;
        let data = if size > 4 {
            let at = u32_of(&entry[8..12]) as usize;
            match usize::try_from(size).ok().and_then(|n| buf.get(at..at.checked_add(n)?)) {
                Some(data) => data,
                None => return tags, // Truncated File Read
            }
        } else {
            &entry[8..8 + size as usize]
        };
        if !data.is_empty() {
            tags.push((tag, typ, data));
        }
    }
    tags
}

/// The first value of an integer-typed field, as Pillow stores a length-1 tag.
fn first_int(typ: u16, data: &[u8], big: bool) -> Option<i64> {
    let get = |n: usize| -> Option<[u8; 8]> {
        let b = data.get(..n)?;
        let mut out = [0u8; 8];
        if big {
            out[8 - n..].copy_from_slice(b);
        } else {
            out[..n].copy_from_slice(b);
        }
        Some(out)
    };
    let unsigned = |n: usize| -> Option<i64> {
        let b = get(n)?;
        let v = if big { u64::from_be_bytes(b) } else { u64::from_le_bytes(b) };
        i64::try_from(v).ok().or(Some(i64::MAX))
    };
    let signed = |n: usize| -> Option<i64> {
        let v = unsigned(n)?;
        let shift = 64 - 8 * n as u32;
        Some((v << shift) >> shift)
    };
    match typ {
        3 => unsigned(2),
        4 | 13 => unsigned(4),
        16 => unsigned(8),
        6 => signed(1),
        8 => signed(2),
        9 => signed(4),
        _ => None,
    }
}

// ---------------------------------------------------------------- PNG

/// `PngImageFile._open`: the chunks up to the first image data, with their
/// checksums and the frame count of an animated PNG.
fn open_png(data: &[u8]) -> Probe<Opened> {
    let mut r = Reader::new(data, 8);
    let (mut width, mut height) = (0u32, 0u32);
    let mut mode: Option<(u8, u8)> = None;
    let mut n_frames: Option<u32> = None;
    let mut seq_num: Option<u32> = None;
    let mut bbox = false;
    let mut default_image = false;
    let mut data_at = None;
    loop {
        let (cid, length) = chunk_header(&mut r)?;
        let body = r.pos;
        match cid {
            b"IDAT" => {
                if !bbox && n_frames.is_some() {
                    default_image = true;
                }
                data_at = Some(body);
                break;
            }
            b"IEND" => break,
            b"fdAT" => {
                if length < 4 {
                    return Err(Broken);
                }
                let seq = be32(r.safe_read(4)?, 0)?;
                if seq_num.is_none_or(|n| i64::from(n) != i64::from(seq) - 1) {
                    return Err(Broken); // APNG contains frame sequence errors
                }
                if !bbox && n_frames.is_some() {
                    default_image = true;
                }
                data_at = Some(body + 4);
                break;
            }
            _ => {}
        }
        let s = r.safe_read(length.into())?;
        match cid {
            b"IHDR" => {
                if length < 13 {
                    return Err(Broken); // Truncated IHDR chunk
                }
                width = be32(s, 0)?;
                height = be32(s, 4)?;
                mode = Some((s[8], s[9]));
                if s[11] != 0 {
                    return Err(Broken); // unknown filter category
                }
            }
            b"tRNS" => match mode.and_then(png_mode) {
                Some("1" | "L" | "I;16") => {
                    be16(s, 0)?;
                }
                Some("RGB") => {
                    be16(s, 4)?;
                }
                _ => {}
            },
            b"gAMA" => {
                be32(s, 0)?;
            }
            b"cHRM" if s.len() % 4 != 0 => return Err(Broken),
            b"sRGB" if length < 1 => return Err(Broken),
            b"pHYs" if length < 9 => return Err(Broken),
            b"zTXt" => {
                let value = s.iter().position(|&b| b == 0).map_or(&[][..], |i| &s[i + 1..]);
                if value.first().is_some_and(|&method| method != 0) {
                    return Err(Broken); // Unknown compression method in zTXt chunk
                }
            }
            b"acTL" => {
                if length < 8 {
                    return Err(Broken);
                }
                if n_frames.is_some() {
                    n_frames = None; // Invalid APNG, will use default PNG image if possible
                } else {
                    let n = be32(s, 0)?;
                    if n != 0 && n <= 0x8000_0000 {
                        n_frames = Some(n);
                    }
                }
            }
            b"fcTL" => {
                if length < 26 {
                    return Err(Broken);
                }
                let seq = be32(s, 0)?;
                let in_order = match seq_num {
                    None => seq == 0,
                    Some(n) => i64::from(n) == i64::from(seq) - 1,
                };
                if !in_order {
                    return Err(Broken); // APNG contains frame sequence errors
                }
                seq_num = Some(seq);
                let (w, h) = (u64::from(be32(s, 4)?), u64::from(be32(s, 8)?));
                let (px, py) = (u64::from(be32(s, 12)?), u64::from(be32(s, 16)?));
                if px + w > u64::from(width) || py + h > u64::from(height) {
                    return Err(Broken); // APNG contains invalid frames
                }
                bbox = true;
            }
            _ => {}
        }
        crc(&mut r, cid, s)?;
    }
    let n_frames = match n_frames {
        Some(n) => u64::from(n) + u64::from(default_image),
        None => 1,
    };
    Ok(Opened { format: Some(Format::Png), width, height, n_frames, data_at })
}

/// Pillow's mode for an IHDR bit depth and colour type, where `tRNS` cares.
fn png_mode((bits, color): (u8, u8)) -> Option<&'static str> {
    Some(match (bits, color) {
        (1, 0) => "1",
        (2 | 4 | 8, 0) => "L",
        (16, 0) => "I;16",
        (8 | 16, 2) => "RGB",
        _ => return None,
    })
}

/// `ChunkStream.read`: length and a four-character `\w` chunk type.
fn chunk_header<'a>(r: &mut Reader<'a>) -> Probe<(&'a [u8], u32)> {
    let s = r.read(8);
    let length = be32(s, 0)?;
    let cid = &s[4..];
    if cid.len() < 4 || !cid.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'_') {
        return Err(Broken); // broken PNG file
    }
    Ok((cid, length))
}

/// `ChunkStream.crc`.
fn crc(r: &mut Reader<'_>, cid: &[u8], body: &[u8]) -> Probe<()> {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(cid);
    hasher.update(body);
    if be32(r.read(4), 0)? != hasher.finalize() {
        return Err(Broken);
    }
    Ok(())
}

/// `PngImageFile.verify`: every chunk from the first image data to IEND has
/// its checksum.
fn verify_png(data: &[u8], data_at: usize) -> Probe<()> {
    let mut r = Reader::new(data, data_at.checked_sub(8).ok_or(Broken)?);
    loop {
        let (cid, length) = chunk_header(&mut r)?;
        if cid == b"IEND" {
            return Ok(());
        }
        let body = r.safe_read(length.into())?;
        crc(&mut r, cid, body)?;
    }
}

// ---------------------------------------------------------------- GIF

/// `GifImageFile._open` and `n_frames`: the screen size grown to the first
/// frame, and the number of frames.
fn open_gif(data: &[u8]) -> Probe<Opened> {
    let mut r = Reader::new(data, 0);
    let s = r.read(13);
    let mut size = (u64::from(le16(s, 6)?), u64::from(le16(s, 8)?));
    let flags = *s.get(10).ok_or(Broken)?;
    if flags & 128 != 0 {
        s.get(11).ok_or(Broken)?;
        palette_needed(r.read(3 << ((flags & 7) + 1)))?;
    }
    let mut gif = GifFrames { r, pending_data: false };
    if !gif.next_frame(Some(&mut size))? {
        return Err(Broken); // image not found in GIF frame
    }
    let mut n_frames = 1u64;
    while gif.next_frame(None)? {
        n_frames += 1;
    }
    let width = u32::try_from(size.0).map_err(|_| Broken)?;
    let height = u32::try_from(size.1).map_err(|_| Broken)?;
    Ok(Opened { format: Some(Format::Gif), width, height, n_frames, data_at: None })
}

struct GifFrames<'a> {
    r: Reader<'a>,
    /// The previous frame's image data still has to be skipped.
    pending_data: bool,
}

impl GifFrames<'_> {
    /// `GifImageFile.data`: one sub-block, `None` at a terminator or the end.
    fn data(&mut self) -> Option<&[u8]> {
        match self.r.byte() {
            Some(n) if n != 0 => Some(self.r.read(n.into())),
            _ => None,
        }
    }

    /// `while self.data(): pass`.
    fn skip_blocks(&mut self) {
        while self.data().is_some_and(|b| !b.is_empty()) {}
    }

    /// `GifImageFile._seek(frame)`: `Ok(false)` where Pillow raises EOFError.
    /// `size` is given for the first frame, whose extent grows the image.
    fn next_frame(&mut self, size: Option<&mut (u64, u64)>) -> Probe<bool> {
        let first = size.is_some();
        if self.pending_data {
            self.skip_blocks();
            self.pending_data = false;
        }
        let mut s = match self.r.byte() {
            None | Some(b';') => return Ok(false), // no more images in GIF file
            b => b,
        };
        loop {
            let c = match s.take().or_else(|| self.r.byte()) {
                None | Some(b';') => return Ok(false),
                Some(c) => c,
            };
            match c {
                b'!' => {
                    let label = self.r.byte();
                    let block = self.data().map(<[u8]>::to_vec);
                    let label = label.ok_or(Broken)?;
                    if label == 249 && block.is_some() {
                        // graphic control extension
                        let block = block.unwrap_or_default();
                        let flags = *block.first().ok_or(Broken)?;
                        if flags & 1 != 0 {
                            block.get(3).ok_or(Broken)?;
                        }
                        le16(&block, 1)?;
                    } else if label == 254 {
                        // comment extension: read up to its terminator, nothing after
                        let mut block = block;
                        while block.as_ref().is_some_and(|b| !b.is_empty()) {
                            block = self.data().map(<[u8]>::to_vec);
                        }
                        continue;
                    } else if label == 255 && first && block.is_some() {
                        // application extension
                        if block.as_ref().is_some_and(|b| b.starts_with(b"NETSCAPE2.0")) {
                            self.data();
                        }
                    }
                    self.skip_blocks();
                }
                b',' => {
                    // local image
                    let s = self.r.read(9);
                    let (x0, y0) = (u64::from(le16(s, 0)?), u64::from(le16(s, 2)?));
                    let (x1, y1) = (x0 + u64::from(le16(s, 4)?), y0 + u64::from(le16(s, 6)?));
                    let flags = *s.get(8).ok_or(Broken)?;
                    if let Some(size) = size {
                        if x1 > size.0 || y1 > size.1 {
                            *size = (x1.max(size.0), y1.max(size.1));
                            bomb_check(size.0, size.1)?;
                        }
                    }
                    if flags & 128 != 0 {
                        palette_needed(self.r.read(3 << ((flags & 7) + 1)))?;
                    }
                    self.r.byte().ok_or(Broken)?; // LZW minimum code size
                    self.pending_data = true;
                    return Ok(true);
                }
                _ => {}
            }
        }
    }
}

/// `GifImageFile._is_palette_needed`, which indexes past a short palette.
fn palette_needed(p: &[u8]) -> Probe<bool> {
    for i in (0..p.len()).step_by(3) {
        if i / 3 != usize::from(p[i]) {
            return Ok(true);
        }
        if p[i] != *p.get(i + 1).ok_or(Broken)? {
            return Ok(true);
        }
        if p[i + 1] != *p.get(i + 2).ok_or(Broken)? {
            return Ok(true);
        }
    }
    Ok(false)
}

// ---------------------------------------------------------------- WebP

/// `WebPImageFile._open`: libwebp's animation decoder gives the canvas size
/// and frame count; a still image is one frame.
fn open_webp(data: &[u8]) -> Probe<Opened> {
    let decoder = image_webp::WebPDecoder::new(Cursor::new(data)).map_err(|_| Broken)?;
    let (width, height) = decoder.dimensions();
    let n_frames = if decoder.is_animated() { decoder.num_frames().into() } else { 1 };
    Ok(Opened { format: Some(Format::Webp), width, height, n_frames, data_at: None })
}

fn load_webp(data: &[u8]) -> Probe<()> {
    let mut decoder = image_webp::WebPDecoder::new(Cursor::new(data)).map_err(|_| Broken)?;
    let size = decoder.output_buffer_size().ok_or(Broken)?;
    let mut buf = vec![0u8; size];
    decoder.read_image(&mut buf).map_err(|_| Broken)
}
