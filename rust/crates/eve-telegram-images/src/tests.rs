//! Port of `bridge/tests/test_telegram_images.py` (the parts that exercise
//! this module), plus the format cases `validate` decides.

use super::*;

use std::io::Cursor;
use std::sync::{Arc, Mutex};

use image::{ImageEncoder, RgbImage};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const TOKEN: &str = "123456:SECRET-token";
const READ: &str = "I couldn't read that image. Send it again as a photo or a still JPEG or PNG.";

fn encode(format: image::ImageFormat, width: u32, height: u32) -> Vec<u8> {
    let picture = RgbImage::from_fn(width, height, |x, y| image::Rgb([(x * 20) as u8, (y * 30) as u8, 0]));
    let mut out = Cursor::new(Vec::new());
    picture.write_to(&mut out, format).expect("encode");
    out.into_inner()
}

/// `png()`: a 12 x 8 red PNG.
fn png() -> Vec<u8> {
    let picture = RgbImage::from_pixel(12, 8, image::Rgb([255, 0, 0]));
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(picture.as_raw(), 12, 8, image::ExtendedColorType::Rgb8)
        .expect("png");
    out
}

fn message(err: &ImageError) -> &str {
    &err.0
}

fn png_chunk(cid: &[u8], body: &[u8]) -> Vec<u8> {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(cid);
    hasher.update(body);
    let mut out = (body.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(cid);
    out.extend_from_slice(body);
    out.extend_from_slice(&hasher.finalize().to_be_bytes());
    out
}

/// Insert a chunk right after IHDR.
fn with_chunk(png: &[u8], cid: &[u8], body: &[u8]) -> Vec<u8> {
    let at = 8 + 25;
    [&png[..at], &png_chunk(cid, body)[..], &png[at..]].concat()
}

// ------------------------------------------------------------ attachment

fn photo() -> Value {
    json!({"message_id": 1, "caption": "What is this?",
           "photo": [{"file_id": "small", "file_size": 10}, {"file_id": "large", "file_size": 100}]})
}

#[test]
fn accept_media_picks_largest_photo_and_image_documents() {
    let item = attachment(&photo()).expect("photo");
    assert_eq!(item, Attachment::with_size("large", 100));
    let doc = json!({"document": {"file_id": "doc", "mime_type": "image/png"}});
    assert_eq!(attachment(&doc), Some(Attachment::new("doc")));
    let text = json!({"document": {"file_id": "doc", "mime_type": "text/plain"}});
    assert_eq!(attachment(&text), None);
    assert_eq!(attachment(&json!({"text": "hi"})), None);
}

#[test]
fn attachment_skips_invalid_sizes_and_prefers_the_later_tie() {
    let msg = json!({"photo": [
        {"file_id": "a", "file_size": 50},
        {"file_id": "b", "file_size": 50},
        {"file_id": "flag", "file_size": true},
        {"file_id": "neg", "file_size": -1},
        {"file_id": "float", "file_size": 900.0},
        {"file_id": "null", "file_size": null},
        {"file_id": "", "file_size": 999},
        {"file_id": 7, "file_size": 999},
        "not a dict",
    ]});
    assert_eq!(attachment(&msg), Some(Attachment::with_size("b", 50)));
    // No sizes at all: Telegram's order decides, so the last one wins.
    let unsized_ = json!({"photo": [{"file_id": "s"}, {"file_id": "m"}]});
    assert_eq!(attachment(&unsized_), Some(Attachment::new("m")));
    // An image document replaces the photo list, even when the photo is bigger.
    let both = json!({"photo": [{"file_id": "p", "file_size": 9}],
                      "document": {"file_id": "d", "mime_type": "image/jpeg", "file_size": 1}});
    assert_eq!(attachment(&both), Some(Attachment::with_size("d", 1)));
    assert_eq!(attachment(&json!({"photo": {"file_id": "x"}})), None);
    assert_eq!(attachment(&json!({"document": {"file_id": "d", "mime_type": null}})), None);
}

#[test]
fn received_image_data_url() {
    let picture = ReceivedImage::new(b"\x89PNG\r\n\x1a\n".to_vec(), "image/png", ".png");
    assert_eq!(picture.data_url(), "data:image/png;base64,iVBORw0KGgo=");
}

// ------------------------------------------------------------ download

/// A one-shot-per-connection HTTP server that answers every request with the
/// same canned response and records request lines.
struct Server {
    base: String,
    requests: Arc<Mutex<Vec<String>>>,
}

async fn serve(status: &str, headers: &[(&str, String)], chunks: Vec<Vec<u8>>) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    let mut head = format!("HTTP/1.1 {status}\r\nconnection: close\r\n");
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else { return };
            let mut request = Vec::new();
            let mut buf = [0u8; 1024];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                match socket.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => request.extend_from_slice(&buf[..n]),
                }
            }
            let line = String::from_utf8_lossy(&request).lines().next().unwrap_or_default().to_string();
            seen.lock().expect("lock").push(line);
            let _ = socket.write_all(head.as_bytes()).await;
            for chunk in &chunks {
                let _ = socket.write_all(chunk).await;
                let _ = socket.flush().await;
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let _ = socket.shutdown().await;
        }
    });
    Server { base, requests }
}

fn metadata(path: &str) -> impl FnOnce(&'static str, Value) -> std::future::Ready<anyhow::Result<Value>> {
    let result = json!({"file_path": path});
    move |_, _| std::future::ready(Ok(result))
}

async fn rejected(server: &Server, limits: Limits, path: &str) -> String {
    let err = download_with(&CLIENT, &server.base, limits, metadata(path), TOKEN, &Attachment::new("f"))
        .await
        .expect_err("must reject");
    let shown = err.downcast_ref::<ImageError>().expect("an ImageError").to_string();
    assert!(!shown.contains(TOKEN));
    shown
}

#[tokio::test]
async fn download_validated_and_token_stays_out() {
    let body = png();
    let server = serve("200 OK", &[("content-length", body.len().to_string())], vec![body.clone()]).await;
    let calls = Arc::new(Mutex::new(Vec::new()));
    let seen = calls.clone();
    let size = body.len();
    let get_file = move |method: &'static str, params: Value| {
        seen.lock().expect("lock").push((method, params));
        std::future::ready(Ok(json!({"file_path": "photos/file_1.png", "file_size": size})))
    };
    let result = download_with(&CLIENT, &server.base, Limits::default(), get_file, TOKEN, &Attachment::new("file"))
        .await
        .expect("download");
    assert_eq!(result.data, png());
    assert_eq!(result.mime, "image/png");
    assert_eq!(result.suffix, ".png");
    assert!(!result.data_url().contains(TOKEN));
    assert_eq!(*calls.lock().expect("lock"), vec![("getFile", json!({"file_id": "file"}))]);
    let requests = server.requests.lock().expect("lock").clone();
    assert_eq!(requests, vec![format!("GET /file/bot{TOKEN}/photos/file_1.png HTTP/1.1")]);
}

#[tokio::test]
async fn reject_bad_downloads() {
    let ok = serve("200 OK", &[], vec![]).await;
    for path in ["../secret.png", "https://evil/image.png", "photos/../x.png", "/photos/x.png", "photos//x.png", "photos/x"] {
        assert_eq!(
            rejected(&ok, Limits::default(), path).await,
            "Telegram returned an invalid image path. Please send it again."
        );
    }
    assert!(ok.requests.lock().expect("lock").is_empty(), "a bad path is never fetched");

    let redirect = serve("302 Found", &[("location", "https://evil".to_string())], vec![]).await;
    assert_eq!(
        rejected(&redirect, Limits::default(), "photos/file.png").await,
        "Telegram couldn't download that image. Please send it again."
    );
    assert_eq!(redirect.requests.lock().expect("lock").len(), 1);

    let junk = serve("200 OK", &[], vec![b"not an image".to_vec()]).await;
    assert_eq!(rejected(&junk, Limits::default(), "photos/file.png").await, READ);

    let huge = serve("200 OK", &[("content-length", (MAX_BYTES + 1).to_string())], vec![]).await;
    assert_eq!(
        rejected(&huge, Limits::default(), "photos/file.png").await,
        "That image is too large. Send an image under 10 MB."
    );
}

#[tokio::test]
async fn size_limits_and_invalid_image() {
    let forbidden = |_: &'static str, _: Value| async { panic!("must reject before download") };
    let err = download(forbidden, TOKEN, &Attachment::with_size("f", MAX_BYTES as u64 + 1))
        .await
        .expect_err("too large");
    assert!(err.downcast_ref::<ImageError>().is_some());

    let stream = serve("200 OK", &[], vec![vec![b'x'; 70], vec![b'x'; 70]]).await;
    let limits = Limits { max_bytes: 100, ..Limits::default() };
    assert!(rejected(&stream, limits, "photos/file.png").await.contains("too large"));

    assert_eq!(message(&validate(b"corrupt").expect_err("corrupt")), READ);
    let limits = Limits { max_bytes: 10 * 1024 * 1024, max_pixels: 10 };
    let err = validate_with(&png(), limits).expect_err("pixels");
    assert!(message(&err).contains("25 megapixels"));
}

#[tokio::test]
async fn metadata_errors() {
    let server = serve("200 OK", &[], vec![png()]).await;
    let run = |result: anyhow::Result<Value>| {
        let base = server.base.clone();
        async move {
            download_with(&CLIENT, &base, Limits::default(), move |_, _| std::future::ready(result), TOKEN, &Attachment::new("f"))
                .await
        }
    };
    // The getFile error itself passes through, not as an ImageError.
    let err = run(Err(anyhow::anyhow!("Bad Request: file is too big"))).await.expect_err("api error");
    assert!(err.downcast_ref::<ImageError>().is_none());
    assert_eq!(err.to_string(), "Bad Request: file is too big");

    let err = run(Ok(json!("nope"))).await.expect_err("not a dict");
    assert_eq!(err.to_string(), "Telegram couldn't provide that image. Please send it again.");
    for size in [json!(-1), json!(1.5), json!(null), json!(MAX_BYTES + 1)] {
        let err = run(Ok(json!({"file_path": "photos/a.png", "file_size": size}))).await.expect_err("size");
        assert_eq!(err.to_string(), "That image is too large. Send an image under 10 MB.");
    }
    // A bool is a Python int, so `true` passes the size check.
    let ok = run(Ok(json!({"file_path": "photos/a.png", "file_size": true}))).await.expect("bool size");
    assert_eq!(ok.data, png());
    let err = run(Ok(json!({"file_size": 3}))).await.expect_err("no path");
    assert_eq!(err.to_string(), "Telegram returned an invalid image path. Please send it again.");
    let err = run(Ok(json!({"file_path": 5}))).await.expect_err("path type");
    assert_eq!(err.to_string(), "Telegram returned an invalid image path. Please send it again.");
}

#[tokio::test]
async fn bad_content_length_and_connection_failures() {
    let odd = serve("200 OK", &[("x-content-length", "1".to_string())], vec![png()]).await;
    assert!(download_with(&CLIENT, &odd.base, Limits::default(), metadata("p/a.png"), TOKEN, &Attachment::new("f"))
        .await
        .is_ok());
    // Nothing listens on this port once the listener is dropped.
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    drop(listener);
    let err = download_with(&CLIENT, &base, Limits::default(), metadata("p/a.png"), TOKEN, &Attachment::new("f"))
        .await
        .expect_err("refused");
    assert_eq!(err.to_string(), "The image download failed. Please send it again.");
    assert!(!format!("{err:?}").contains(TOKEN));
}

// ------------------------------------------------------------ save

fn mode(path: &Path) -> u32 {
    fs::metadata(path).expect("stat").permissions().mode() & 0o777
}

#[test]
fn private_files_and_expiry() {
    let tmp = tempfile::tempdir().expect("tmp");
    let root = tmp.path().join("images");
    let image = validate(&png()).expect("valid");
    let path = save(&image, Some(&root)).expect("save");
    assert_eq!(fs::read(&path).expect("read"), png());
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(&root), 0o700);
    assert!(path.is_absolute() && path == fs::canonicalize(&path).expect("canonical"));
    let old = SystemTime::now() - Duration::from_secs(KEEP_SECS + 1);
    fs::File::options().write(true).open(&path).expect("open").set_modified(old).expect("utime");
    let new = save(&image, Some(&root)).expect("save again");
    assert!(!path.exists() && new.exists());
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&root, &link).expect("symlink");
    let err = save(&image, Some(&link)).expect_err("symlinked folder");
    assert_eq!(err.downcast_ref::<ImageError>().map(|e| e.0.as_str()), Some("The private image folder is unavailable."));
}

#[test]
fn shared_folder_is_refused_and_full_folder_is_bounded() {
    let tmp = tempfile::tempdir().expect("tmp");
    let root = tmp.path().join("images");
    fs::create_dir(&root).expect("mkdir");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).expect("chmod");
    let image = validate(&png()).expect("valid");
    let err = save(&image, Some(&root)).expect_err("group-readable");
    assert_eq!(err.to_string(), "The private image folder is unavailable.");

    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("chmod");
    for _ in 0..99 {
        save(&image, Some(&root)).expect("room");
    }
    // Files that are not ours do not count.
    fs::write(root.join("notes.txt"), b"x").expect("write");
    fs::write(root.join(format!("{}.bmp", "a".repeat(32))), b"x").expect("write");
    save(&image, Some(&root)).expect("the hundredth");
    let err = save(&image, Some(&root)).expect_err("full");
    assert_eq!(err.to_string(), "The temporary image folder is full. Try again after older images expire.");

    let file = tmp.path().join("plain-file");
    fs::write(&file, b"x").expect("write");
    let err = save(&image, Some(&file)).expect_err("a file, not a folder");
    assert!(err.downcast_ref::<ImageError>().is_none());
}

#[test]
fn default_root_is_per_user_in_the_temp_dir() {
    let root = default_root();
    assert_eq!(root.file_name().and_then(|n| n.to_str()), Some(format!("eve-telegram-images-{}", uid()).as_str()));
    assert!(root.is_absolute());
    assert!(abspath(Path::new("/a/./b/../c/")) == Path::new("/a/c"));
}

// ------------------------------------------------------------ validate

#[test]
fn still_formats_are_accepted_with_their_type() {
    let cases = [
        (image::ImageFormat::Jpeg, "image/jpeg", ".jpg"),
        (image::ImageFormat::Png, "image/png", ".png"),
        (image::ImageFormat::Gif, "image/gif", ".gif"),
        (image::ImageFormat::WebP, "image/webp", ".webp"),
    ];
    for (format, mime, suffix) in cases {
        let data = encode(format, 40, 30);
        let image = validate(&data).unwrap_or_else(|e| panic!("{format:?}: {e}"));
        assert_eq!((image.mime.as_str(), image.suffix.as_str()), (mime, suffix));
        assert_eq!(image.data, data);
    }
}

#[test]
fn other_formats_and_animations_ask_for_a_still() {
    let still = "Send a still JPEG, PNG, WebP, or GIF image.";
    for format in [image::ImageFormat::Bmp, image::ImageFormat::Tiff] {
        assert_eq!(message(&validate(&encode(format, 4, 4)).expect_err("other")), still);
    }
    // Two-frame GIF.
    let mut gif = Vec::new();
    {
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut gif);
        for colour in [[255, 0, 0, 255], [0, 0, 255, 255]] {
            let frame = image::RgbaImage::from_pixel(4, 4, image::Rgba(colour));
            encoder.encode_frame(image::Frame::new(frame)).expect("frame");
        }
    }
    assert_eq!(message(&validate(&gif).expect_err("animated gif")), still);
    // acTL with no frame control before the image data: the default image is
    // an extra frame, so even one declared frame makes two.
    let animated = with_chunk(&png(), b"acTL", &[0, 0, 0, 1, 0, 0, 0, 0]);
    assert_eq!(message(&validate(&animated).expect_err("apng")), still);
    // A zero frame count is ignored, as is a duplicated acTL.
    assert!(validate(&with_chunk(&png(), b"acTL", &[0; 8])).is_ok());
    let twice = with_chunk(&with_chunk(&png(), b"acTL", &[0, 0, 0, 3, 0, 0, 0, 0]), b"acTL", &[0, 0, 0, 3, 0, 0, 0, 0]);
    assert!(validate(&twice).is_ok());
}

/// A JPEG with an MPF segment naming `count` pictures (little-endian index).
fn multi_picture(count: u32, entries: usize) -> Vec<u8> {
    let jpeg = encode(image::ImageFormat::Jpeg, 16, 16);
    let mut mpf = b"MPF\0II\x2a\x00\x08\x00\x00\x00".to_vec();
    let entry_offset = 8 + 2 + 2 * 12 + 4;
    mpf.extend_from_slice(&2u16.to_le_bytes());
    mpf.extend_from_slice(&0xB001u16.to_le_bytes());
    mpf.extend_from_slice(&4u16.to_le_bytes());
    mpf.extend_from_slice(&1u32.to_le_bytes());
    mpf.extend_from_slice(&count.to_le_bytes());
    mpf.extend_from_slice(&0xB002u16.to_le_bytes());
    mpf.extend_from_slice(&7u16.to_le_bytes());
    mpf.extend_from_slice(&((entries * 16) as u32).to_le_bytes());
    mpf.extend_from_slice(&(entry_offset as u32).to_le_bytes());
    mpf.extend_from_slice(&0u32.to_le_bytes());
    for _ in 0..entries {
        mpf.extend_from_slice(&0x0003_0000u32.to_le_bytes());
        mpf.extend_from_slice(&[0; 12]);
    }
    let mut segment = vec![0xFF, 0xE2];
    segment.extend_from_slice(&((mpf.len() + 2) as u16).to_be_bytes());
    segment.extend_from_slice(&mpf);
    [&jpeg[..2], &segment[..], &jpeg[2..]].concat()
}

#[test]
fn multi_picture_jpeg_is_not_a_still() {
    assert_eq!(
        message(&validate(&multi_picture(2, 2)).expect_err("mpo")),
        "Send a still JPEG, PNG, WebP, or GIF image."
    );
    assert!(validate(&multi_picture(1, 1)).is_ok());
    // An entry list shorter than the count: Pillow cannot open the file.
    assert_eq!(message(&validate(&multi_picture(3, 1)).expect_err("short index")), READ);
    // Ultra HDR gain maps stay plain JPEGs.
    let xmp = b"http://ns.adobe.com/xap/1.0/\x00<x hdrgm:Version=\"1.0\"/>";
    let mut app1 = vec![0xFF, 0xE1];
    app1.extend_from_slice(&((xmp.len() + 2) as u16).to_be_bytes());
    app1.extend_from_slice(xmp);
    let mpo = multi_picture(2, 2);
    let hdr = [&mpo[..2], &app1[..], &mpo[2..]].concat();
    assert!(validate(&hdr).is_ok());
}

#[test]
fn truncated_and_corrupt_images_are_unreadable() {
    let jpeg = encode(image::ImageFormat::Jpeg, 40, 30);
    // Missing only the end-of-image marker is enough.
    for cut in [2, 20, jpeg.len() / 2] {
        assert_eq!(message(&validate(&jpeg[..jpeg.len() - cut]).expect_err("jpeg")), READ);
    }
    let mut trailing = jpeg.clone();
    trailing.extend_from_slice(b"after the end");
    assert!(validate(&trailing).is_ok());

    let png = png();
    for cut in [12, 30, png.len() / 2] {
        assert_eq!(message(&validate(&png[..png.len() - cut]).expect_err("png")), READ);
    }
    let mut bad_crc = png.clone();
    bad_crc[20] ^= 0xFF;
    assert_eq!(message(&validate(&bad_crc).expect_err("crc")), READ);
    let mut bad_data = png.clone();
    let at = bad_data.len() - 20;
    bad_data[at] ^= 0xFF;
    assert_eq!(message(&validate(&bad_data).expect_err("late crc")), READ);
    assert_eq!(message(&validate(&with_chunk(&png, b"gAMA", &[0, 1])).expect_err("short gAMA")), READ);

    let gif = encode(image::ImageFormat::Gif, 40, 30);
    // Without its trailer the frame is whole; half the file is not.
    assert!(validate(&gif[..gif.len() - 1]).is_ok());
    assert_eq!(message(&validate(&gif[..gif.len() / 2]).expect_err("gif")), READ);

    let webp = encode(image::ImageFormat::WebP, 40, 30);
    assert_eq!(message(&validate(&webp[..webp.len() / 2]).expect_err("webp")), READ);
    assert_eq!(message(&validate(b"").expect_err("empty")), READ);
}

#[test]
fn pixel_limit_and_decompression_bombs() {
    let gif = encode(image::ImageFormat::Gif, 40, 30);
    let with_screen = |w: u16, h: u16| {
        let mut g = gif.clone();
        g[6..8].copy_from_slice(&w.to_le_bytes());
        g[8..10].copy_from_slice(&h.to_le_bytes());
        g
    };
    assert!(message(&validate(&with_screen(6000, 5000)).expect_err("26 MP")).contains("25 megapixels"));
    // Past Pillow's bomb limit the open itself fails.
    assert_eq!(message(&validate(&with_screen(65535, 65535)).expect_err("bomb")), READ);
    // A screen smaller than the frame grows to the frame.
    assert!(validate(&with_screen(10, 10)).is_ok());

    let jpeg = encode(image::ImageFormat::Jpeg, 40, 30);
    let sof = jpeg.windows(2).position(|w| w == [0xFF, 0xC0]).expect("SOF0");
    let mut big = jpeg.clone();
    big[sof + 5..sof + 7].copy_from_slice(&5000u16.to_be_bytes());
    big[sof + 7..sof + 9].copy_from_slice(&6000u16.to_be_bytes());
    assert!(message(&validate(&big).expect_err("big jpeg")).contains("25 megapixels"));

    assert_eq!(
        message(&validate(&vec![0u8; MAX_BYTES + 1]).expect_err("bytes")),
        "That image is too large. Send an image under 10 MB."
    );
}
