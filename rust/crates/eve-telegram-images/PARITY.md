# eve-telegram-images
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/telegram_images.py` | `src/lib.rs` (public API), `src/probe.rs` (Pillow's open/verify/load decisions) |

## API
| Python | Rust | Notes |
|---|---|---|
| `MAX_BYTES` | `MAX_BYTES` | 10 MiB |
| `MAX_PIXELS` | `MAX_PIXELS` | 25 000 000 |
| `KEEP_SECS` | `KEEP_SECS` | 24 h |
| `FORMATS` | `FORMATS` | `[(pillow name, mime, suffix); 4]`, same order |
| `class ImageError(Exception)` | `struct ImageError(pub String)` (`thiserror`) | `download` and `save` return `anyhow::Error`; callers test `err.downcast_ref::<ImageError>()` where Python catches `ImageError`. Other errors (the `getFile` call, filesystem) pass through as in Python. |
| `class Attachment` (frozen dataclass) | `struct Attachment { file_id, size: u64 }` | `Attachment::new(id)` is `Attachment(id)` (size 0); `Attachment::with_size` |
| `class ReceivedImage` (frozen dataclass) | `struct ReceivedImage { data, mime, suffix }` | |
| `ReceivedImage.data_url` | `ReceivedImage::data_url` | `data:<mime>;base64,<standard base64>` |
| `attachment(message)` | `attachment(&Value) -> Option<Attachment>` | Same candidate rules: photo list, an `image/*` document replaces it, `file_id` non-empty string, `file_size` a non-bool int >= 0 (default 0); max by (size, index) |
| `validate(data)` | `validate(&[u8])`, `validate_with(&[u8], Limits)` | Same three messages in the same precedence; see `probe.rs` and Differences |
| `async download(client, get_file, token, item)` | `download(get_file, token, &item)`, `download_with(&client, base, Limits, get_file, token, &item)` | `get_file` is `FnOnce("getFile", {"file_id": …}) -> Future<anyhow::Result<Value>>`. Same checks and messages; GET `https://api.telegram.org/file/bot<token>/<path>`, no redirects, 30 s connect/read, streamed with the 10 MB cap, validation on a blocking thread |
| `save(image, root=None)` | `save(&ReceivedImage, Option<&Path>) -> anyhow::Result<PathBuf>`, `default_root()` | Same folder (`gettempdir()/eve-telegram-images-<uid>`, 0700, owner-only, no symlink), same expiry (mtime older than 24 h, names `[0-9a-f]{32}\.(jpg|png|webp|gif)`, regular files), same 100-file bound, `O_EXCL` file chmod 0600, resolved path returned |
| (Pillow `Image.open`) | `probe::open` | Header walkers that follow Pillow 12's `JpegImagePlugin._open` + `jpeg_factory`/`_getmp`, `PngImageFile._open`, `GifImageFile._open` + `n_frames`, `WebPImageFile._open`, and `_decompression_bomb_check` |
| (Pillow `verify()` + `load()`) | `probe::verify_and_load` | PNG chunk checksums from the first IDAT to IEND, JPEG end-of-image reached, then a full decode with the `image` crate (`image-webp` for WebP) |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_telegram_images.py` | `test_accept_media_and_owner_guards` | `accept_media_picks_largest_photo_and_image_documents`, `attachment_skips_invalid_sizes_and_prefers_the_later_tie` | The owner, chat-type, age, bot and forward guards are `telegram.accept`, ported with the telegram crate |
| | `test_download_validated_and_token_stays_out_of_model` | `download_validated_and_token_stays_out` | The model-request and history assertions are `telegram` inlet behaviour |
| | `test_reject_bad_downloads` (5 cases) | `reject_bad_downloads` | |
| | `test_size_limits_and_invalid_image` | `size_limits_and_invalid_image` | `monkeypatch` of `MAX_BYTES`/`MAX_PIXELS` becomes a `Limits` argument |
| | `test_private_files_and_expiry` | `private_files_and_expiry` | |
| | `test_route_image_to_selected_session_with_caption`, `test_image_caption_cannot_approve_or_run_commands`, `test_changed_relay_does_not_receive_download`, `test_a_relayed_image_is_seen_then_delivered_as_reactions`, `test_an_image_that_is_not_delivered_takes_its_eyes_off`, `test_an_image_whose_eyes_are_refused_is_still_delivered_with_the_line` | — | Telegram inlet routing, relays and reactions: they belong to the telegram crate |
| (new) | — | `received_image_data_url`, `metadata_errors`, `bad_content_length_and_connection_failures`, `shared_folder_is_refused_and_full_folder_is_bounded`, `default_root_is_per_user_in_the_temp_dir`, `still_formats_are_accepted_with_their_type`, `other_formats_and_animations_ask_for_a_still`, `multi_picture_jpeg_is_not_a_still`, `truncated_and_corrupt_images_are_unreadable`, `pixel_limit_and_decompression_bombs` | |
| `bridge/tests/test_tool_gate.py` (ref) | builds `ReceivedImage(b"\x89PNG…", "image/png", ".png")` | `received_image_data_url` | The rest is tool-gate behaviour |
| `bridge/tests/test_telegram.py` (ref) | `validate` of a PNG screenshot | `still_formats_are_accepted_with_their_type` | The rest is telegram behaviour |
| `bridge/tests/test_link_reactions.py` (ref) | uses `Attachment`/`validate` to drive the inlet | covered by the tests above | Reactions are telegram behaviour |

`validate` was also checked differentially against the live Python (Pillow 12.3, the bridge venv) on 90 generated files: still, animated, multi-picture, truncated, corrupted, oversized and bomb-sized JPEG/PNG/GIF/WebP, plus BMP/TIFF/ICO/PPM/QOI/PCX/TGA. Every file got the same outcome except the rows listed under Differences.

## Boundaries
None.

## Differences
- `download` has no `client` argument. reqwest sets redirects per client, not per request, so `download` uses a module client that never follows redirects (Python passes `follow_redirects=False` on the request). `download_with` takes a client, a file-server base and `Limits` for tests or a local Bot API server.
- Where an exception escapes Python's `validate` instead of an `ImageError` (a PNG with a bad checksum or a malformed chunk after its image data raises `SyntaxError`; a PNG with no image data raises `IndexError`; a GIF with no image raises `EOFError`; a malformed later GIF frame raises `struct.error`/`IndexError`), Rust returns the "couldn't read" `ImageError`.
- A format that Pillow opens but the `image` crate does not recognise (PCX, TGA, PSD, AVIF/HEIC and other plugins) gets "I couldn't read that image…" instead of "Send a still JPEG, PNG, WebP, or GIF image." Both refuse the file.
- Pixel decoding is the `image` crate's (zune-jpeg, png, gif, image-webp), not libjpeg/zlib/giflib/libwebp. Arithmetic-coded JPEGs, which libjpeg-turbo decodes, are refused as unreadable. On corrupt entropy data the two decoders may disagree in rare cases.
- Pillow refuses a PNG whose compressed text chunk (`zTXt`/`iTXt`) inflates past 1 MB; that bomb check is not repeated (an unknown `zTXt` compression method is still refused).
- JSON integers outside the 64-bit range count as non-integers in `attachment` and `download`.
- If no temporary directory is writable, Python's `gettempdir()` raises; Rust falls back to `/tmp` and `save` then fails on it.
