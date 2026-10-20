# eve-link-uploads
Status: complete

## Modules
| Python | Rust |
|---|---|
| `eve_bridge/link_uploads.py` | `src/lib.rs` (store, classify, served, brain parts), `src/office.rs` (Office and RTF text for eve lite), `src/mimetypes.rs` + `src/mimetable.rs` (Python's `mimetypes.guess_type`, which `keep` calls), `src/py.rs` (the Python built-ins the module leans on: `str.strip`, `os.path.splitext`, `Path.suffix`, `int()`, `json.dumps`, `unicodedata.category`, `tempfile.gettempdir`) |

## API
| Python | Rust | Notes |
|---|---|---|
| `MAX_BYTES`, `KEEP_SECS`, `MAX_KEPT`, `MAX_EVE_KEPT`, `MAX_EVE_BYTES`, `MAX_NAME_CHARS`, `THUMB_PX`, `MAX_TEXT_CHARS` | same names | Same values |
| `ID_RE`, `SUFFIX_RE`, `_FILE_RE`, `_MIME_RE` | `ID_RE`, `SUFFIX_RE`, `FILE_RE`, `MIME_RE` | Python's `$` (end, or before one final newline) is written `\n?\z` |
| `META` | `META` | `.meta.json` |
| `TEXT_SUFFIXES`, `OFFICE_SUFFIXES` | `TEXT_SUFFIXES`, `OFFICE_SUFFIXES` (ordered slices), `office_mime` | |
| `KINDS`, `ACTIVE_MIMES`, `INLINE_KINDS` | same names | |
| `served(kind, mime)` | `served(&str, &str) -> (String, &'static str)` | |
| `class UploadError(Exception)` | `struct UploadError { message, status }` (`thiserror`), `UploadError::new` (415), `with_status` | Store methods return `StoreError::{Upload, Io}`: Python raises `UploadError` or lets `OSError` through; `StoreError::upload()` is the `except UploadError` test |
| `class Upload` (frozen dataclass) | `struct Upload { id, kind, name, mime, size: u64, path }` | `PartialEq` as the dataclass's `==` |
| `Upload.public` | `Upload::public -> Value` | Same keys, same order |
| `clean_name(raw, fallback)` | `clean_name` | Counts and cuts by code points as Python does |
| `classify(data, name)` | `classify`, `classify_with(data, name, max_bytes)` | Same checks, order, kinds, messages and statuses; the image check is `eve_telegram_images::validate`. `classify_with` is the seam for the tests that patch `MAX_BYTES` |
| `_thumb(data)` | `thumb`, `make_thumb`, `thumb_size` | EXIF orientation applied, Pillow's `thumbnail` size rule (`round_aspect`), non-RGB/L converted to RGB, JPEG quality 72; same warning `link uploads: no thumbnail (<class>)` |
| `_write(path, data)` | `write_private` | `O_CREAT|O_EXCL|O_NOFOLLOW|O_CLOEXEC`, 0600, then `fchmod` 0600 |
| `class UploadStore` | `struct UploadStore` | `UploadStore::new(root)` + `with_clock`, `with_keep_secs`, `with_max_kept`, `with_max_eve_kept` (`max(1, n)`), `with_max_eve_bytes`; `with_max_bytes` replaces patching the module's `MAX_BYTES`. Default root `gettempdir()/eve-link-uploads-<uid>` (`default_root`) |
| `UploadStore._folder` | `UploadStore::folder` | mkdir 0700 (`exist_ok`: any error but a missing parent is fine when a directory is there), refused (500, "The private upload folder is unavailable.") unless a real directory owned by this uid with no group/other bits |
| `UploadStore.sweep` / `_sweep` | `sweep` / `sweep_alive` + `sweep_listed` | Same rule: anything not named `<id><suffix>`, `<id>.meta.json`, `<id>.thumb.jpg`, not a regular file, or older than `keep_secs` is unlinked when it is a file or a link; a file gone between the listing and the lstat is skipped |
| `UploadStore._census` | `census` | Same: an unreadable or odd metadata counts as the owner's; eve's copies sorted by (written, id, size) |
| `UploadStore._drop` | `drop_upload` | Every name starting with the id |
| `UploadStore.save` | `save` | |
| `UploadStore.keep` | `keep` | A 415 refusal becomes kind `file`, suffix from the name when it matches `SUFFIX_RE` else `.bin`, type from `mimetypes::guess_type("file"+suffix)` or `application/octet-stream`; metadata `"from": "eve"` |
| `UploadStore._store` | `store` | Same caps (503 "Too many attachments are waiting. Try again after older ones expire."), eve's oldest dropped first, same file order (data, thumb, metadata last), same log lines; the metadata is written as `json.dumps` writes it (`", "`/`": "` separators, `\uXXXX` for non-ASCII) |
| `UploadStore.get(upload_id: object)` | `get(&str)`, `get_value(&Value)` | `get_value` is the `object` case: a non-string is None |
| `UploadStore.thumb_url` | `thumb_url` | `data:image/jpeg;base64,…` |
| `image_of(upload)` | `image_of -> io::Result<ReceivedImage>` | suffix is `Path.suffix` |
| `_as_context(name, body, mime)` | `as_context` | Same wording; cut at `MAX_TEXT_CHARS` code points with `\n[… cut: the file goes on]` |
| `brain_parts(upload)` | `brain_parts`, `brain_parts_in(upload, &EnvMap)` | Same three wordings verbatim; `profile.is_lite()` is `eve_profile::is_lite_in` (a bad `EVE_PROFILE` errs, as Python raises `ProfileError`); an `office_text` failure logs `link uploads: <kind> (<id>) could not be read (<class>)` and reads as no words |
| `relay_line(upload)` | `relay_line` | |
| `OFFICE_MAX_UNZIPPED`, `_XML_UNSAFE`, `_WORD_T`, `_WORD_P`, `_DRAW_T`, `_SHEET_NS` | `OFFICE_MAX_UNZIPPED`, `XML_UNSAFE`, `WORD_NS`, `DRAW_NS`, `SHEET_NS` | |
| `office_text(upload)` | `office_text -> anyhow::Result<String>` | Errs where Python raises (not a zip, unreadable file, an exception a member read lets out: `PyException`); `error_name` gives the Python class for the log line (`BadZipFile`, `FileNotFoundError`, `error`, `LZMAError`, `RuntimeError`, …) |
| `_xml_root(data)` | `xml_text` + `parse` (roxmltree) | Refuses any `<!DOCTYPE`/`<!ENTITY` (case-insensitive) before parsing; UTF-8 (BOM optional) and UTF-16 with a BOM |
| `_zip_xml_text(path, extract)` | `zip_xml_text`, `stream_error_name` | Same running budget over the declared uncompressed sizes; a member is never inflated past its declared size. A member read is None where Python's `read` catches (`KeyError`, `BadZipFile`: a bad local header or a CRC mismatch, `OSError`, bzip2's errors) and fails the whole document where it does not (encrypted: `RuntimeError`; unsupported method: `NotImplementedError`; corrupt deflate: `zlib.error`; corrupt LZMA: `LZMAError`); after such a failure every later read is None, as Python's exception stops `extract` |
| `_docx_from(read)` | `docx_from` | `t.text` is `et_text`: ElementTree's `.text`, the text before the first child element, joined across comments and processing instructions (ElementTree drops them; roxmltree keeps them as nodes) |
| `_pptx_from(read)` | `pptx_from` | |
| `_col(ref)` | `col` | `str.isalpha` is `py::is_alpha` (categories L*), not Rust's `is_alphabetic` |
| `_xlsx_from(read)` | `xlsx_from` | Same (len, name) column order, same `str(len(cells))` key for a cell with no reference |
| `_cell_value(cell, shared)` | `cell_value` | `int()` and list indexing as Python (whitespace, sign, underscores, any Unicode decimal digit, a negative index from the end); `v.text` is `et_text` |
| `_RTF_WORD` | `control_word` | Matched in place, as the Python does since its quadratic fix |
| `_rtf_text(data)` | `rtf_text` | latin-1, same state machine, same `MAX_TEXT_CHARS + 1` bound on pieces |

## Tests
| Python file | Python tests | Rust tests | Not ported (why) |
|---|---|---|---|
| `bridge/tests/test_link_uploads.py` | `test_the_store_keeps_files_private_and_names_them_by_id_only` | `the_store_keeps_files_private_and_names_them_by_id_only` | |
| | `test_a_shared_or_foreign_folder_is_refused` | `a_shared_or_foreign_folder_is_refused`, `a_symlinked_folder_is_refused` | |
| | `test_ids_are_32_lowercase_hex_and_nothing_else` (10 cases) | `ids_are_32_lowercase_hex_and_nothing_else` | `None`, `7` and a list go through `get_value` |
| | `test_an_id_never_stored_is_none_and_a_stray_file_in_the_folder_is_swept` | same name, plus `a_stray_directory_is_left_and_a_stray_link_is_swept` | |
| | `test_uploads_expire_after_a_day_and_are_deleted` | same name | |
| | `test_what_it_takes` (6), `test_what_it_refuses` (9) | `what_it_takes`, `what_it_refuses`, `refusals_carry_the_owners_message_and_status` | |
| | `test_every_text_type_it_takes_can_be_read_back_and_survives_a_sweep` (4) | same name | |
| | `test_a_file_deleted_while_the_sweep_lists_the_folder_is_skipped` | same name | `Path.iterdir` patch becomes the `sweep_listed` seam fed a listing whose first file is then deleted |
| | `test_rtf_text_reads_control_words_in_place` | `rtf_text_reads_control_words_in_place`, `rtf_text_drops_tables_and_keeps_escapes` | The spy on `re.match` copies has no Rust analogue: the matcher reads a `&[char]` in place by construction; the output assertions are kept |
| | `test_upload_needs_the_link_token_and_no_origin`, `test_upload_size_cap_and_type_refusal`, `test_a_turn_names_only_known_live_ids`, `test_attachments_reach_the_inlet_and_an_empty_text_is_fine_with_one`, `test_photos_and_pdfs_over_the_inline_cap_together_are_refused_before_the_brain`, `test_an_image_turn_reaches_the_brain_as_an_image_like_a_telegram_photo`, `test_a_pdf_and_a_text_file_reach_the_brain_as_the_file_and_its_words`, `test_an_expired_attachment_shows_as_gone_in_the_history`, `test_a_bad_att_on_a_transcript_line_is_left_out`, `test_with_the_claude_relay_on_an_app_image_is_saved_and_its_path_typed`, `test_an_attachment_is_not_taken_as_the_answer_to_a_waiting_question`, `test_an_owner_lines_attachment_note_is_not_sent_to_the_app` | `a_pdf_and_a_text_file_reach_the_brain_as_the_file_and_its_words` (the `brain_parts` half), `image_of_is_the_telegram_photo_shape`, `a_full_doc_upload_is_a_mac_task_line` (`relay_line`) | The HTTP door, the turn route, the transcript rows and the inlet are link_chat.py and telegram.py: they belong to those crates |
| `bridge/tests/test_app_media.py` (ref) | `test_keep_takes_what_save_refuses_but_not_empty_or_too_large`, `test_the_file_door_serves_only_a_photo_or_pdf_inline_and_never_an_active_type` (8), `test_served_maps_every_active_or_malformed_type_to_a_download`, `test_eves_copies_have_their_own_cap_and_never_refuse_the_owners_upload`, `test_eves_copies_make_room_by_bytes_too` | same names, plus `keep_guesses_a_files_type_as_python_mimetypes_does` | The door test is checked at the store: `keep` then `served` of what it kept; the HTTP headers are link_chat's. The rest of the file is telegram/link_chat behaviour |
| `bridge/tests/test_lite_brain.py` (ref) | `test_a_lite_doc_upload_reaches_the_brain_as_its_words_not_a_mac_task` | same name (at `brain_parts_in` with a lite environment), `a_lite_doc_with_no_words_says_so`, `a_lite_rtf_upload_is_read` | The lite inlet itself is the lite crate's |
| `bridge/tests/test_link_reactions.py`, `test_lite_link_channel.py`, `test_lite_web_local.py` (ref) | build an `UploadStore` for the door | — | They test link_chat and lite_web |
| (new) | — | `a_corrupt_deflate_member_fails_the_document_and_a_crc_mismatch_reads_as_absent`, `xml_text_is_elementtree_text_and_numbers_read_as_int_does` (both expectations checked against the bridge venv), `the_metadata_is_what_python_writes`, `a_python_written_upload_is_read_back`, `clean_name_keeps_the_last_part_and_cuts_long_names`, `a_big_photo_gets_a_small_thumbnail`, `a_long_text_file_is_cut`, `xlsx_cells_keep_their_columns_and_shared_strings`, `pptx_slides_in_order_split_by_a_blank_line`, `a_part_that_declares_a_dtd_or_entity_is_not_read`, `the_unzipped_budget_bounds_what_is_read` | |

The port was also checked differentially against the live Python (bridge venv, Python 3.13) once during porting: `clean_name` on 19 hostile names, `classify` on 18 byte/name pairs (kinds, types, suffixes, messages, statuses), `mimetypes.guess_type` on 1 048 suffixes (every one Python knows on this Mac, plus the encoding and suffix-map cases), `office_text` on 7 hand-built documents (nested tables, CDATA, UTF-16, a broken part, shared strings with phonetic runs, `int("1_0")` and negative indexes, a non-zip) and `_rtf_text` on 300 random inputs. All agreed.

## Boundaries
None.

## Differences
- The thumbnail is made by the `image` crate (CatmullRom resize, its JPEG encoder at quality 72), not Pillow (draft + reduce + bicubic, libjpeg): the same size and orientation, different bytes.
- RTF: Python's text can hold a lone UTF-16 surrogate (`\u-10179?` with its pair after a `?`); a Rust string cannot, so a surrogate half not directly followed by its other half becomes U+FFFD, and two halves in a row become the one character they encode (what the model gets once Python's JSON is decoded).
- A metadata file that is valid JSON but not an object raises `AttributeError` out of Python's `get`; Rust's `get` returns None. A non-string `name`/`mime` value is shown with JSON's spelling rather than Python's `str()`.
- Office parts: an XML part in an encoding other than UTF-8 or BOM-marked UTF-16 (a declared `ISO-8859-1`, say) is read as unparseable (Python's expat would decode it). Zip methods read: stored, deflate, bzip2, lzma. Damaged zips, at the edges: a member whose local header cannot be read is None without being charged to the unzipped budget (the zip crate gives its size only through that header); a local header whose file name differs from the directory's is read (Python's `BadZipFile`, None); an encrypted member with a damaged local header fails the document (Python: None); an I/O error from the disk in the middle of inflating a deflate or LZMA member can fail the document where Python's `OSError` reads as None.
- `mimetypes`: the `mime.types` files are read as UTF-8 and skipped when they are not (Python's `init` would raise).
- If no temporary directory is writable, Python's `gettempdir()` raises; Rust falls back to `/tmp` and the store's first mkdir then fails on it.
