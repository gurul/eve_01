//! The text codecs `protocol.py` hands to `str.encode` / `bytes.decode`.
//!
//! The CJK firmware builds read string bytes in gbk, big5 or shift_jis. The
//! Python daemon uses CPython's codecs for those, and CPython's tables are not
//! the WHATWG ones (WHATWG GBK adds the euro sign and gb18030 extras, WHATWG
//! Big5 is Big5-HKSCS, WHATWG Shift_JIS is windows-31j), so a WHATWG encoder
//! would put characters on the wire that the Python daemon turns into `?`.
//! The tables under `data/` are CPython's, written by
//! `tools/gen_codec_tables.py`.

use thiserror::Error;

/// Python's `LookupError` from the codec registry.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown encoding: {0}")]
pub struct LookupError(pub String);

/// A codec the wire protocol can use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Codec {
    Utf8,
    Gbk,
    Big5,
    ShiftJis,
}

struct Tables {
    /// 5-byte records: code point (3 bytes BE), encoding (2 bytes BE, 0x00XX
    /// for one byte). Sorted by code point. Identity ASCII is left out.
    encode: &'static [u8],
    /// 6-byte records: code point, the code point its encoding decodes to.
    roundtrip: &'static [u8],
}

macro_rules! table {
    ($name:literal) => {
        Tables {
            encode: include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/data/", $name, ".bin")),
            roundtrip: include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/data/", $name, ".roundtrip.bin")),
        }
    };
}

static GBK: Tables = table!("gbk");
static BIG5: Tables = table!("big5");
static SHIFT_JIS: Tables = table!("shift_jis");

fn read_u24(b: &[u8]) -> u32 {
    (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2])
}

/// Binary search a sorted table of `width`-byte records keyed by a 3-byte code point.
fn find(table: &[u8], width: usize, cp: u32) -> Option<&[u8]> {
    let (mut lo, mut hi) = (0usize, table.len() / width);
    while lo < hi {
        let mid = (lo + hi) / 2;
        let rec = &table[mid * width..(mid + 1) * width];
        match read_u24(rec).cmp(&cp) {
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Greater => hi = mid,
            std::cmp::Ordering::Equal => return Some(&rec[3..]),
        }
    }
    None
}

/// CPython's encoding-name normalization: `_Py_normalize_encoding` (ASCII
/// alphanumerics and `.` kept and lowercased, every other run of bytes becomes
/// one `_`, none leading or trailing), as `codecs.lookup` applies it.
fn normalize(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut punct = false;
    for &b in name.as_bytes() {
        if b.is_ascii_alphanumeric() || b == b'.' {
            if punct && !out.is_empty() {
                out.push('_');
            }
            punct = false;
            out.push(b.to_ascii_lowercase() as char);
        } else {
            punct = true;
        }
    }
    out
}

/// `encodings.aliases.aliases` entries for the codecs this crate carries.
const ALIASES: &[(&str, &str)] = &[
    ("936", "gbk"),
    ("cp936", "gbk"),
    ("ms936", "gbk"),
    ("big5_tw", "big5"),
    ("csbig5", "big5"),
    ("x_mac_trad_chinese", "big5"),
    ("csshiftjis", "shift_jis"),
    ("s_jis", "shift_jis"),
    ("shiftjis", "shift_jis"),
    ("sjis", "shift_jis"),
    ("x_mac_japanese", "shift_jis"),
    ("cp65001", "utf_8"),
    ("u8", "utf_8"),
    ("utf", "utf_8"),
    ("utf8", "utf_8"),
    ("utf8_ucs2", "utf_8"),
    ("utf8_ucs4", "utf_8"),
];

fn alias(norm: &str) -> Option<&'static str> {
    ALIASES.iter().find(|(k, _)| *k == norm).map(|(_, v)| *v)
}

fn module(name: &str) -> Option<Codec> {
    match name {
        "utf_8" => Some(Codec::Utf8),
        "gbk" => Some(Codec::Gbk),
        "big5" => Some(Codec::Big5),
        "shift_jis" => Some(Codec::ShiftJis),
        _ => None,
    }
}

impl Codec {
    /// `codecs.lookup(name)`, as `encodings.search_function` resolves it:
    /// the alias of the normalized name first (also with `.` read as `_`),
    /// then the normalized name itself; a name holding `.` is never a module.
    pub fn lookup(name: &str) -> Result<Codec, LookupError> {
        let norm = normalize(name);
        let aliased = alias(&norm).or_else(|| alias(&norm.replace('.', "_")));
        aliased
            .into_iter()
            .chain(std::iter::once(norm.as_str()))
            .filter(|m| !m.is_empty() && !m.contains('.'))
            .find_map(module)
            .ok_or_else(|| LookupError(name.to_owned()))
    }

    /// The codec's canonical Python name (`codecs.lookup(x).name`).
    pub fn name(self) -> &'static str {
        match self {
            Codec::Utf8 => "utf-8",
            Codec::Gbk => "gbk",
            Codec::Big5 => "big5",
            Codec::ShiftJis => "shift_jis",
        }
    }

    fn tables(self) -> Option<&'static Tables> {
        match self {
            Codec::Utf8 => None,
            Codec::Gbk => Some(&GBK),
            Codec::Big5 => Some(&BIG5),
            Codec::ShiftJis => Some(&SHIFT_JIS),
        }
    }

    /// Append the encoding of `c` to `out`; false when the codec cannot represent it.
    fn encode_char_into(self, c: char, out: &mut Vec<u8>) -> bool {
        let Some(t) = self.tables() else {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            return true;
        };
        let cp = u32::from(c);
        if cp < 0x80 {
            // The generator proves ASCII encodes to itself in all three codecs.
            out.push(cp as u8);
            return true;
        }
        match find(t.encode, 5, cp) {
            Some(code) if code[0] == 0 => {
                out.push(code[1]);
                true
            }
            Some(code) => {
                out.extend_from_slice(code);
                true
            }
            None => false,
        }
    }

    /// The encoding of one character, or `None` when the codec has no bytes for it.
    pub fn encode_char(self, c: char) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(4);
        self.encode_char_into(c, &mut out).then_some(out)
    }

    /// `s.encode(codec, errors="replace")`: each unencodable code point becomes `?`.
    pub fn encode_replace(self, s: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(s.len());
        for c in s.chars() {
            if !self.encode_char_into(c, &mut out) {
                out.push(b'?');
            }
        }
        out
    }

    /// `s.encode(codec, errors="replace").decode(codec, errors="replace")`.
    ///
    /// These codecs are stateless and every encoding is a whole character, so
    /// the round trip is per character: an unencodable one becomes `?`, an
    /// encodable one becomes what its bytes decode to (shift_jis reads the
    /// bytes of U+00A5 and U+203E back as `\` and `~`).
    pub fn roundtrip_replace(self, s: &str) -> String {
        let Some(t) = self.tables() else {
            return s.to_owned();
        };
        let mut out = String::with_capacity(s.len());
        let mut scratch = Vec::with_capacity(4);
        for c in s.chars() {
            scratch.clear();
            if !self.encode_char_into(c, &mut scratch) {
                out.push('?');
                continue;
            }
            match find(t.roundtrip, 6, u32::from(c)).and_then(|b| char::from_u32(read_u24(b))) {
                Some(back) => out.push(back),
                None => out.push(c),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(table: &[u8], width: usize) -> bool {
        table.chunks(width).collect::<Vec<_>>().windows(2).all(|w| read_u24(w[0]) < read_u24(w[1]))
    }

    #[test]
    fn tables_are_whole_records_and_sorted() {
        for c in [Codec::Gbk, Codec::Big5, Codec::ShiftJis] {
            let t = c.tables().unwrap();
            assert_eq!(t.encode.len() % 5, 0, "{c:?}");
            assert_eq!(t.roundtrip.len() % 6, 0, "{c:?}");
            assert!(sorted(t.encode, 5), "{c:?}");
            assert!(sorted(t.roundtrip, 6), "{c:?}");
        }
        // Counts CPython 3.13 reports (non-ASCII mappings).
        assert_eq!(GBK.encode.len() / 5, 21791);
        assert_eq!(BIG5.encode.len() / 5, 13706);
        assert_eq!(SHIFT_JIS.encode.len() / 5, 6944);
    }

    #[test]
    fn cpython_table_values() {
        assert_eq!(Codec::Gbk.encode_char('你'), Some(vec![0xC4, 0xE3]));
        assert_eq!(Codec::Gbk.encode_char('好'), Some(vec![0xBA, 0xC3]));
        // CPython's gbk has no euro sign (WHATWG GBK maps it to 0x80).
        assert_eq!(Codec::Gbk.encode_char('€'), None);
        assert_eq!(Codec::Big5.encode_char('¥'), Some(vec![0xA2, 0x44]));
        assert_eq!(Codec::Big5.encode_char('\u{401}'), Some(vec![0xC7, 0xB3]));
        assert_eq!(Codec::ShiftJis.encode_char('¥'), Some(vec![0x5C]));
        assert_eq!(Codec::ShiftJis.encode_char('¢'), Some(vec![0x81, 0x91]));
        // windows-31j extras are not in CPython's shift_jis.
        assert_eq!(Codec::ShiftJis.encode_char('\u{2160}'), None);
        assert_eq!(Codec::ShiftJis.encode_char('ｱ'), Some(vec![0xB1]));
        assert_eq!(Codec::Gbk.encode_char('🎮'), None);
    }

    #[test]
    fn shift_jis_roundtrip_overrides() {
        assert_eq!(Codec::ShiftJis.roundtrip_replace("¥‾"), "\\~");
        assert_eq!(Codec::Gbk.roundtrip_replace("你🎮"), "你?");
        assert_eq!(Codec::Utf8.roundtrip_replace("你🎮"), "你🎮");
    }

    #[test]
    fn encode_replace_marks_each_code_point() {
        assert_eq!(Codec::Gbk.encode_replace("你🎮好"), b"\xC4\xE3?\xBA\xC3".to_vec());
        // A ZWJ sequence is three code points, so three replacements.
        assert_eq!(Codec::Gbk.encode_replace("👩\u{200d}👩"), b"???".to_vec());
    }

    #[test]
    fn lookup_matches_cpython() {
        let ok = [
            ("GBK", Codec::Gbk),
            (" gbk ", Codec::Gbk),
            ("gbk\u{e9}", Codec::Gbk),
            ("cp936", Codec::Gbk),
            ("--gbk--", Codec::Gbk),
            ("Shift-JIS", Codec::ShiftJis),
            ("shift jis", Codec::ShiftJis),
            ("SJIS", Codec::ShiftJis),
            ("s.jis", Codec::ShiftJis),
            ("big5-tw", Codec::Big5),
            ("BIG5", Codec::Big5),
            ("utf8", Codec::Utf8),
            ("UTF-8", Codec::Utf8),
            ("u8", Codec::Utf8),
        ];
        for (name, want) in ok {
            assert_eq!(Codec::lookup(name), Ok(want), "{name:?}");
        }
        for name in ["g-b-k", "gb k", "shift.jis", "GBK.", "x-sjis", "g\u{e9}bk", "bogus", ""] {
            assert_eq!(Codec::lookup(name), Err(LookupError(name.to_owned())), "{name:?}");
        }
        assert_eq!(LookupError("bogus".into()).to_string(), "unknown encoding: bogus");
    }

    /// Regenerates the tables with the bridge's Python and compares bytes.
    #[test]
    #[ignore = "live: needs the bridge's Python (EVE_PROTOCOL_PYTHON or python3)"]
    fn tables_match_cpython() {
        let python = std::env::var("EVE_PROTOCOL_PYTHON").unwrap_or_else(|_| "python3".into());
        let dir = tempfile::tempdir().unwrap();
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tools/gen_codec_tables.py");
        let st = std::process::Command::new(python).arg("-I").arg(script).arg(dir.path()).status().unwrap();
        assert!(st.success());
        for name in ["gbk", "big5", "shift_jis"] {
            for suffix in [".bin", ".roundtrip.bin"] {
                let fresh = std::fs::read(dir.path().join(format!("{name}{suffix}"))).unwrap();
                let shipped =
                    std::fs::read(format!("{}/data/{name}{suffix}", env!("CARGO_MANIFEST_DIR"))).unwrap();
                assert!(fresh == shipped, "{name}{suffix} differs from CPython");
            }
        }
    }
}
