//! The pending-byte state of CPython's incremental decoders, as
//! `_codec_safe_chunks` reads it through `decoder.getstate()[0]`.
//!
//! Only the question "is a character unfinished after this byte?" matters to
//! the chunker, so this models the decoders' buffering and nothing else. The
//! CJK tables are CPython's, written and proven by
//! `tools/gen_decoder_tables.py`; the UTF-8 rules are the ones
//! `codecs.utf_8_decode(..., final=False)` applies when it keeps a tail.

use eve_protocol::Codec;

/// One codec's automaton: which bytes start a two-byte character, and which
/// (lead, trail) pairs complete one.
pub(crate) struct DbcsTable(&'static [u8]);

impl DbcsTable {
    fn starts(&self, b: u8) -> bool {
        bit(&self.0[..32], usize::from(b))
    }

    fn completes(&self, lead: u8, trail: u8) -> bool {
        debug_assert!(lead >= 0x80);
        bit(&self.0[32..], usize::from(lead - 0x80) * 256 + usize::from(trail))
    }
}

fn bit(bits: &[u8], i: usize) -> bool {
    bits[i / 8] & (1 << (i % 8)) != 0
}

macro_rules! table {
    ($name:literal) => {
        DbcsTable(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/data/", $name, ".dec.bin")))
    };
}

static GBK: DbcsTable = table!("gbk");
static BIG5: DbcsTable = table!("big5");
static SHIFT_JIS: DbcsTable = table!("shift_jis");

/// `codecs.getincrementaldecoder(codec)(errors="replace")`, reduced to its
/// pending buffer.
pub(crate) enum IncrementalDecoder {
    /// CPython's buffered UTF-8 decoder: the tail kept is a valid prefix of
    /// one sequence.
    Utf8 { pending: Vec<u8> },
    /// The multibyte CJK decoders: at most one pending lead byte.
    Dbcs { table: &'static DbcsTable, pending: Option<u8> },
}

impl IncrementalDecoder {
    pub(crate) fn new(codec: Codec) -> Self {
        match codec {
            Codec::Utf8 => IncrementalDecoder::Utf8 { pending: Vec::with_capacity(4) },
            Codec::Gbk => IncrementalDecoder::Dbcs { table: &GBK, pending: None },
            Codec::Big5 => IncrementalDecoder::Dbcs { table: &BIG5, pending: None },
            Codec::ShiftJis => IncrementalDecoder::Dbcs { table: &SHIFT_JIS, pending: None },
        }
    }

    /// `decoder.decode(bytes([b]))`.
    pub(crate) fn feed(&mut self, b: u8) {
        match self {
            IncrementalDecoder::Utf8 { pending } => utf8_feed(pending, b),
            IncrementalDecoder::Dbcs { table, pending } => {
                *pending = match *pending {
                    Some(lead) if table.completes(lead, b) => None,
                    // No pending lead, or the pair is not a character: CPython
                    // replaces the lead and reads this byte afresh.
                    _ => table.starts(b).then_some(b),
                };
            }
        }
    }

    /// `bool(decoder.getstate()[0])`.
    pub(crate) fn has_pending(&self) -> bool {
        match self {
            IncrementalDecoder::Utf8 { pending } => !pending.is_empty(),
            IncrementalDecoder::Dbcs { pending, .. } => pending.is_some(),
        }
    }
}

/// Length of the sequence a UTF-8 lead byte starts, or `None` for a byte
/// CPython reports as an invalid start byte (or ASCII, which never pends).
fn utf8_len(lead: u8) -> Option<usize> {
    match lead {
        0xC2..=0xDF => Some(2),
        0xE0..=0xEF => Some(3),
        0xF0..=0xF4 => Some(4),
        _ => None,
    }
}

/// Whether `b` may follow `pending` inside one sequence. The second byte is
/// range-checked for E0/ED/F0/F4 (no overlongs, surrogates, or code points
/// past U+10FFFF), the rest are plain continuation bytes.
fn utf8_continues(pending: &[u8], b: u8) -> bool {
    if pending.len() == 1 {
        let range = match pending[0] {
            0xE0 => 0xA0..=0xBF,
            0xED => 0x80..=0x9F,
            0xF0 => 0x90..=0xBF,
            0xF4 => 0x80..=0x8F,
            _ => 0x80..=0xBF,
        };
        range.contains(&b)
    } else {
        (0x80..=0xBF).contains(&b)
    }
}

fn utf8_feed(pending: &mut Vec<u8>, b: u8) {
    if !pending.is_empty() {
        if utf8_continues(pending, b) {
            pending.push(b);
            if Some(pending.len()) == utf8_len(pending[0]) {
                pending.clear();
            }
            return;
        }
        // The held prefix is replaced as one error; this byte starts afresh.
        pending.clear();
    }
    if utf8_len(b).is_some() {
        pending.push(b);
    }
}
