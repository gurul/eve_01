//! Python's `mimetypes.guess_type` for a file name, as `UploadStore::keep` asks it: Python 3.13's built-in strict
//! table, then every `mime.types` file in `mimetypes.knownfiles` that exists (on macOS, `/etc/apache2/mime.types`),
//! each later entry replacing an earlier one, read once per process.

use std::collections::HashMap;
use std::sync::LazyLock;

use crate::mimetable::TYPES_MAP_DEFAULT;
use crate::py;

/// `mimetypes.knownfiles`, in order.
pub const KNOWN_FILES: &[&str] = &[
    "/etc/mime.types",
    "/etc/httpd/mime.types",
    "/etc/httpd/conf/mime.types",
    "/etc/apache/mime.types",
    "/etc/apache2/mime.types",
    "/usr/local/etc/httpd/conf/mime.types",
    "/usr/local/lib/netscape/mime.types",
    "/usr/local/etc/httpd/conf/mime.types",
    "/usr/local/etc/mime.types",
];

const SUFFIX_MAP: &[(&str, &str)] = &[
    (".svgz", ".svg.gz"),
    (".tgz", ".tar.gz"),
    (".taz", ".tar.gz"),
    (".tz", ".tar.gz"),
    (".tbz2", ".tar.bz2"),
    (".txz", ".tar.xz"),
];
const ENCODINGS_MAP: &[(&str, &str)] =
    &[(".gz", "gzip"), (".Z", "compress"), (".bz2", "bzip2"), (".xz", "xz"), (".br", "br")];

static TYPES: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
    let mut types: HashMap<String, String> =
        TYPES_MAP_DEFAULT.iter().map(|(ext, mime)| (ext.to_string(), mime.to_string())).collect();
    for file in KNOWN_FILES {
        // a file that is missing or not UTF-8 adds nothing (Python's init would raise on the latter)
        if let Ok(text) = std::fs::read_to_string(file) {
            read_mime_types(&text, &mut types);
        }
    }
    types
});

/// `MimeTypes.readfp` (universal newlines): each line's first word is a type, the rest its suffixes; `#` starts a comment.
pub fn read_mime_types(text: &str, types: &mut HashMap<String, String>) {
    for line in text.split(['\r', '\n']) {
        let mut words: Vec<&str> = line.split(py::is_space).filter(|w| !w.is_empty()).collect();
        if let Some(i) = words.iter().position(|w| w.starts_with('#')) {
            words.truncate(i);
        }
        let Some((mime, suffixes)) = words.split_first() else { continue };
        for suffix in suffixes {
            types.insert(format!(".{suffix}"), mime.to_string());
        }
    }
}

fn lookup(table: &[(&'static str, &'static str)], key: &str) -> Option<&'static str> {
    table.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// `mimetypes.guess_type(path)` (strict) for a plain file name or path: (type, encoding).
pub fn guess_type(path: &str) -> (Option<String>, Option<&'static str>) {
    guess_type_in(path, &TYPES)
}

/// [`guess_type`] against a given strict table.
pub fn guess_type_in(path: &str, types: &HashMap<String, String>) -> (Option<String>, Option<&'static str>) {
    let (mut base, mut ext) = py::splitext(path);
    while let Some(longer) = lookup(SUFFIX_MAP, &ext.to_lowercase()) {
        (base, ext) = py::splitext(&format!("{base}{longer}"));
    }
    // encodings_map is case sensitive
    let encoding = lookup(ENCODINGS_MAP, &ext);
    if encoding.is_some() {
        (_, ext) = py::splitext(&base);
    }
    (types.get(&ext.to_lowercase()).cloned(), encoding)
}
