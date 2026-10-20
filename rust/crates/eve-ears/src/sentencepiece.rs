//! The part of `sentencepiece` that `ears.encode_phrase` uses: load a `.model` file and encode a string to
//! pieces (`SentencePieceProcessor.encode(text, out_type=str)`).
//!
//! The Python daemon calls the C++ library through its wheel. There is no cmake on the Mac to build the
//! C++ binding crate, so this is a reimplementation of the same algorithms, read from sentencepiece's
//! `normalizer.cc`, `unigram_model.cc`, `bpe_model.cc`, `model_interface.cc` and
//! `sentencepiece_processor.cc`:
//!
//! - the protobuf `ModelProto` (pieces, trainer spec, normalizer spec), parsed by hand;
//! - the normalizer: the precompiled character map (a darts-clone double array over the input bytes, the
//!   longest rule wins), user-defined symbols passed through untouched, extra whitespace removed, the
//!   dummy prefix, `" "` escaped as `"▁"`;
//! - the unigram model's optimized Viterbi (`EncodeOptimized`), with the user-defined-symbol bonus and the
//!   unknown penalty of 10 below the lowest score;
//! - the BPE model's best-pair merge loop, with re-segmentation of unused pieces;
//! - the character model;
//! - the processor's merge of consecutive unknown pieces, and byte fallback.
//!
//! The word model is left out (loading one is an error): a keyword spotter never uses one for its tokens.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};

const SPACE_SYMBOL: &str = "\u{2581}";
/// unigram_model.h: `kUnkPenalty`.
const UNK_PENALTY: f32 = 10.0;
/// darts-clone `commonPrefixSearch` result buffer in `Normalizer::NormalizePrefix`.
const MAX_TRIE_RESULTS: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PieceType {
    Normal,
    Unknown,
    Control,
    UserDefined,
    Unused,
    Byte,
}

impl PieceType {
    fn from_proto(v: u64) -> PieceType {
        match v {
            2 => PieceType::Unknown,
            3 => PieceType::Control,
            4 => PieceType::UserDefined,
            5 => PieceType::Unused,
            6 => PieceType::Byte,
            _ => PieceType::Normal,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModelType {
    Unigram,
    Bpe,
    Word,
    Char,
}

#[derive(Clone, Debug)]
struct Piece {
    piece: String,
    score: f32,
    kind: PieceType,
}

#[derive(Clone, Debug)]
struct NormalizerSpec {
    name: String,
    precompiled_charsmap: Vec<u8>,
    add_dummy_prefix: bool,
    remove_extra_whitespaces: bool,
    escape_whitespaces: bool,
}

impl Default for NormalizerSpec {
    fn default() -> Self {
        NormalizerSpec {
            name: String::new(),
            precompiled_charsmap: Vec::new(),
            add_dummy_prefix: true,
            remove_extra_whitespaces: true,
            escape_whitespaces: true,
        }
    }
}

// ---- protobuf (wire format only) ---------------------------------------------------

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

enum Field<'a> {
    Varint(u64),
    Fixed64,
    Bytes(&'a [u8]),
    Fixed32(u32),
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    fn varint(&mut self) -> Result<u64> {
        let mut out: u64 = 0;
        for shift in (0..64).step_by(7) {
            let b = *self.buf.get(self.pos).ok_or_else(|| anyhow!("truncated varint"))?;
            self.pos += 1;
            out |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(out);
            }
        }
        bail!("varint too long")
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.buf.len()).ok_or_else(|| anyhow!("truncated field"))?;
        let out = &self.buf[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    fn next(&mut self) -> Result<Option<(u64, Field<'a>)>> {
        if self.pos >= self.buf.len() {
            return Ok(None);
        }
        let key = self.varint()?;
        let field = key >> 3;
        let value = match key & 7 {
            0 => Field::Varint(self.varint()?),
            1 => {
                self.take(8)?;
                Field::Fixed64
            }
            2 => {
                let n = usize::try_from(self.varint()?)?;
                Field::Bytes(self.take(n)?)
            }
            5 => {
                let b = self.take(4)?;
                Field::Fixed32(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            }
            w => bail!("unsupported protobuf wire type {w}"),
        };
        Ok(Some((field, value)))
    }
}

fn parse_piece(buf: &[u8]) -> Result<Piece> {
    let mut p = Piece { piece: String::new(), score: 0.0, kind: PieceType::Normal };
    let mut r = Reader::new(buf);
    while let Some((field, value)) = r.next()? {
        match (field, value) {
            (1, Field::Bytes(b)) => p.piece = String::from_utf8_lossy(b).into_owned(),
            (2, Field::Fixed32(v)) => p.score = f32::from_bits(v),
            (3, Field::Varint(v)) => p.kind = PieceType::from_proto(v),
            _ => {}
        }
    }
    Ok(p)
}

struct TrainerSpec {
    model_type: ModelType,
    treat_whitespace_as_suffix: bool,
    byte_fallback: bool,
}

fn parse_trainer_spec(buf: &[u8]) -> Result<TrainerSpec> {
    let mut t = TrainerSpec { model_type: ModelType::Unigram, treat_whitespace_as_suffix: false, byte_fallback: false };
    let mut r = Reader::new(buf);
    while let Some((field, value)) = r.next()? {
        match (field, value) {
            (3, Field::Varint(v)) => {
                t.model_type = match v {
                    2 => ModelType::Bpe,
                    3 => ModelType::Word,
                    4 => ModelType::Char,
                    _ => ModelType::Unigram,
                }
            }
            (24, Field::Varint(v)) => t.treat_whitespace_as_suffix = v != 0,
            (35, Field::Varint(v)) => t.byte_fallback = v != 0,
            _ => {}
        }
    }
    Ok(t)
}

fn parse_normalizer_spec(buf: &[u8]) -> Result<NormalizerSpec> {
    let mut n = NormalizerSpec::default();
    let mut r = Reader::new(buf);
    while let Some((field, value)) = r.next()? {
        match (field, value) {
            (1, Field::Bytes(b)) => n.name = String::from_utf8_lossy(b).into_owned(),
            (2, Field::Bytes(b)) => n.precompiled_charsmap = b.to_vec(),
            (3, Field::Varint(v)) => n.add_dummy_prefix = v != 0,
            (4, Field::Varint(v)) => n.remove_extra_whitespaces = v != 0,
            (5, Field::Varint(v)) => n.escape_whitespaces = v != 0,
            _ => {}
        }
    }
    Ok(n)
}

// ---- darts-clone double array ---------------------------------------------------------

struct DoubleArray {
    units: Vec<u32>,
}

impl DoubleArray {
    fn has_leaf(unit: u32) -> bool {
        (unit >> 8) & 1 == 1
    }

    fn value(unit: u32) -> u32 {
        unit & ((1u32 << 31) - 1)
    }

    fn label(unit: u32) -> u32 {
        unit & ((1u32 << 31) | 0xFF)
    }

    fn offset(unit: u32) -> usize {
        ((unit >> 10) << ((unit & (1u32 << 9)) >> 6)) as usize
    }

    /// `commonPrefixSearch(key, results, 32, len)`: (value, length) of every key in the trie that is a
    /// prefix of `key`, shortest first, at most 32 of them.
    fn common_prefix_search(&self, key: &[u8]) -> Vec<(u32, usize)> {
        let mut out = Vec::new();
        let Some(&first) = self.units.first() else { return out };
        let mut node_pos = Self::offset(first);
        for (i, &b) in key.iter().enumerate() {
            node_pos ^= b as usize;
            let Some(&unit) = self.units.get(node_pos) else { return out };
            if Self::label(unit) != u32::from(b) {
                return out;
            }
            node_pos ^= Self::offset(unit);
            if Self::has_leaf(unit) {
                let Some(&leaf) = self.units.get(node_pos) else { return out };
                if out.len() < MAX_TRIE_RESULTS {
                    out.push((Self::value(leaf), i + 1));
                }
            }
        }
        out
    }
}

/// string_util.h `OneCharLen`: the UTF-8 length announced by the lead byte.
fn one_char_len(b: u8) -> usize {
    const LEN: [usize; 16] = [1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 3, 4];
    LEN[(b >> 4) as usize]
}

/// `PrefixMatcher`: the longest user-defined symbol at the start of `w`, or one character.
struct PrefixMatcher {
    symbols: Vec<Vec<u8>>,
}

impl PrefixMatcher {
    fn prefix_match(&self, w: &[u8]) -> (usize, bool) {
        let longest = self
            .symbols
            .iter()
            .filter(|s| !s.is_empty() && w.starts_with(s))
            .map(|s| s.len())
            .max()
            .unwrap_or(0);
        if longest > 0 {
            (longest, true)
        } else {
            (w.len().min(one_char_len(w[0])), false)
        }
    }
}

// ---- the processor ----------------------------------------------------------------

/// A loaded `.model` file: `SentencePieceProcessor(model_file=...)`.
pub struct SentencePieceProcessor {
    pieces: Vec<Piece>,
    /// NORMAL, USER_DEFINED and UNUSED pieces (model_interface.cc `pieces_`).
    piece_ids: HashMap<Vec<u8>, usize>,
    /// UNKNOWN, CONTROL and BYTE pieces (`reserved_id_map_`).
    reserved_ids: HashMap<Vec<u8>, usize>,
    max_piece_len: usize,
    unk_id: Option<usize>,
    model_type: ModelType,
    treat_whitespace_as_suffix: bool,
    byte_fallback: bool,
    normalizer: NormalizerSpec,
    trie: Option<DoubleArray>,
    normalized_blob: Vec<u8>,
    matcher: PrefixMatcher,
    min_score: f32,
    max_score: f32,
}

impl SentencePieceProcessor {
    /// Load a serialized `ModelProto` file.
    pub fn open(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
        Self::from_bytes(&bytes)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let mut pieces = Vec::new();
        let mut trainer = TrainerSpec { model_type: ModelType::Unigram, treat_whitespace_as_suffix: false, byte_fallback: false };
        let mut normalizer = NormalizerSpec::default();
        let mut r = Reader::new(bytes);
        while let Some((field, value)) = r.next()? {
            match (field, value) {
                (1, Field::Bytes(b)) => pieces.push(parse_piece(b)?),
                (2, Field::Bytes(b)) => trainer = parse_trainer_spec(b)?,
                (3, Field::Bytes(b)) => normalizer = parse_normalizer_spec(b)?,
                _ => {}
            }
        }
        if pieces.is_empty() {
            bail!("sentencepiece model has no pieces");
        }
        if trainer.model_type == ModelType::Word {
            bail!("sentencepiece word models are not supported");
        }
        let mut piece_ids = HashMap::new();
        let mut reserved_ids = HashMap::new();
        let mut user_defined = Vec::new();
        let mut unk_id = None;
        let mut max_piece_len = 0;
        for (i, p) in pieces.iter().enumerate() {
            if p.piece.is_empty() {
                bail!("piece must not be empty");
            }
            let normal = matches!(p.kind, PieceType::Normal | PieceType::UserDefined | PieceType::Unused);
            let map = if normal { &mut piece_ids } else { &mut reserved_ids };
            if map.insert(p.piece.as_bytes().to_vec(), i).is_some() {
                bail!("{} is already defined", p.piece);
            }
            if normal {
                max_piece_len = max_piece_len.max(p.piece.len());
            }
            if p.kind == PieceType::UserDefined {
                user_defined.push(p.piece.as_bytes().to_vec());
            }
            if p.kind == PieceType::Unknown {
                if unk_id.is_some() {
                    bail!("unk is already defined");
                }
                unk_id = Some(i);
            }
        }
        let unk_id = unk_id.ok_or_else(|| anyhow!("unk is not defined"))?;
        // unigram_model.cc: FLT_MIN is the smallest positive float, so max_score_ never drops below it.
        let mut min_score = f32::MAX;
        let mut max_score = f32::MIN_POSITIVE;
        for p in pieces.iter().filter(|p| p.kind == PieceType::Normal) {
            min_score = min_score.min(p.score);
            max_score = max_score.max(p.score);
        }
        let (trie, normalized_blob) = decode_charsmap(&normalizer.precompiled_charsmap)?;
        Ok(SentencePieceProcessor {
            pieces,
            piece_ids,
            reserved_ids,
            max_piece_len,
            unk_id: Some(unk_id),
            model_type: trainer.model_type,
            treat_whitespace_as_suffix: trainer.treat_whitespace_as_suffix,
            byte_fallback: trainer.byte_fallback,
            normalizer,
            trie,
            normalized_blob,
            matcher: PrefixMatcher { symbols: user_defined },
            min_score,
            max_score,
        })
    }

    /// `encode(text, out_type=str)`.
    pub fn encode(&self, text: &str) -> Vec<String> {
        let normalized = self.normalize(text.as_bytes());
        if normalized.is_empty() {
            return Vec::new();
        }
        let result = match self.model_type {
            ModelType::Unigram => self.encode_unigram(&normalized),
            ModelType::Bpe => self.encode_bpe(&normalized),
            ModelType::Char | ModelType::Word => self.encode_char(&normalized),
        };
        // sentencepiece_processor.cc PopulateSentencePieceText: consecutive unknown pieces merge into one.
        let mut out: Vec<(Vec<u8>, bool)> = Vec::new();
        for (w, id) in result {
            if w.is_empty() {
                continue;
            }
            let is_unk = self.is_unknown(id);
            if is_unk && self.byte_fallback {
                for b in &w {
                    let piece = format!("<0x{b:02X}>");
                    out.push((piece.into_bytes(), false));
                }
                continue;
            }
            match out.last_mut() {
                Some((last, true)) if is_unk => last.extend_from_slice(&w),
                _ => out.push((w, is_unk)),
            }
        }
        out.into_iter().map(|(w, _)| String::from_utf8_lossy(&w).into_owned()).collect()
    }

    fn is_unknown(&self, id: Option<usize>) -> bool {
        match id {
            Some(i) => self.pieces[i].kind == PieceType::Unknown,
            None => true,
        }
    }

    fn piece_to_id(&self, w: &[u8]) -> Option<usize> {
        self.reserved_ids.get(w).or_else(|| self.piece_ids.get(w)).copied().or(self.unk_id)
    }

    // -- normalizer.cc --

    fn normalize_prefix<'a>(&'a self, input: &'a [u8]) -> (&'a [u8], usize) {
        let (mblen, found) = self.matcher.prefix_match(input);
        if found {
            return (&input[..mblen], mblen);
        }
        let mut longest_length = 0;
        let mut longest_value = 0usize;
        if let Some(trie) = &self.trie {
            for (value, length) in trie.common_prefix_search(input) {
                if longest_length == 0 || length > longest_length {
                    longest_length = length;
                    longest_value = value as usize;
                }
            }
        }
        if longest_length == 0 {
            // The input is a &str, so a character is always well formed here.
            let len = utf8_len(input);
            (&input[..len], len)
        } else {
            let tail = self.normalized_blob.get(longest_value..).unwrap_or(&[]);
            let end = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
            (&tail[..end], longest_length)
        }
    }

    fn normalize(&self, mut input: &[u8]) -> Vec<u8> {
        let spec = &self.normalizer;
        let mut out = Vec::new();
        if input.is_empty() {
            return out;
        }
        if spec.remove_extra_whitespaces {
            while !input.is_empty() {
                let (sp, n) = self.normalize_prefix(input);
                if sp != b" " {
                    break;
                }
                input = &input[n..];
            }
        }
        if input.is_empty() {
            return out;
        }
        let space: &[u8] = if spec.escape_whitespaces { SPACE_SYMBOL.as_bytes() } else { b" " };
        if !self.treat_whitespace_as_suffix && spec.add_dummy_prefix {
            out.extend_from_slice(space);
        }
        let mut is_prev_space = spec.remove_extra_whitespaces;
        while !input.is_empty() {
            let (mut sp, n) = self.normalize_prefix(input);
            if is_prev_space {
                while let Some(rest) = sp.strip_prefix(b" ") {
                    sp = rest;
                }
            }
            if !sp.is_empty() {
                for &b in sp {
                    if spec.escape_whitespaces && b == b' ' {
                        out.extend_from_slice(SPACE_SYMBOL.as_bytes());
                    } else {
                        out.push(b);
                    }
                }
                is_prev_space = sp.ends_with(b" ");
            }
            input = &input[n..];
        }
        if spec.remove_extra_whitespaces {
            while out.ends_with(space) {
                out.truncate(out.len() - space.len());
            }
        }
        if self.treat_whitespace_as_suffix && spec.add_dummy_prefix {
            out.extend_from_slice(space);
        }
        out
    }

    // -- unigram_model.cc EncodeOptimized --

    fn encode_unigram(&self, normalized: &[u8]) -> Vec<(Vec<u8>, Option<usize>)> {
        #[derive(Clone, Copy)]
        struct BestPathNode {
            id: Option<usize>,
            best_path_score: f32,
            starts_at: Option<usize>,
        }
        let size = normalized.len();
        let unk_score = self.min_score - UNK_PENALTY;
        let mut best = vec![BestPathNode { id: None, best_path_score: 0.0, starts_at: None }; size + 1];
        let mut starts_at = 0;
        while starts_at < size {
            let till_here = best[starts_at].best_path_score;
            let mut has_single_node = false;
            let mblen = one_char_len(normalized[starts_at]).min(size - starts_at);
            let longest = self.max_piece_len.min(size - starts_at);
            for key_end in starts_at + 1..=starts_at + longest {
                let Some(&id) = self.piece_ids.get(&normalized[starts_at..key_end]) else { continue };
                let kind = self.pieces[id].kind;
                if kind == PieceType::Unused {
                    continue;
                }
                let length = key_end - starts_at;
                // User defined symbol receives extra bonus to always be selected.
                let score = if kind == PieceType::UserDefined {
                    length as f32 * self.max_score - 0.1
                } else {
                    self.pieces[id].score
                };
                let candidate = score + till_here;
                let target = &mut best[key_end];
                if target.starts_at.is_none() || candidate > target.best_path_score {
                    target.best_path_score = candidate;
                    target.starts_at = Some(starts_at);
                    target.id = Some(id);
                }
                if !has_single_node && length == mblen {
                    has_single_node = true;
                }
            }
            if !has_single_node {
                let candidate = unk_score + till_here;
                let target = &mut best[starts_at + mblen];
                if target.starts_at.is_none() || candidate > target.best_path_score {
                    target.best_path_score = candidate;
                    target.starts_at = Some(starts_at);
                    target.id = self.unk_id;
                }
            }
            starts_at += mblen;
        }
        let mut results = Vec::new();
        let mut ends_at = size;
        while ends_at > 0 {
            let node = best[ends_at];
            let Some(start) = node.starts_at else { break };
            results.push((normalized[start..ends_at].to_vec(), node.id));
            ends_at = start;
        }
        results.reverse();
        results
    }

    // -- bpe_model.cc Encode --

    fn encode_bpe(&self, normalized: &[u8]) -> Vec<(Vec<u8>, Option<usize>)> {
        struct Symbol {
            prev: Option<usize>,
            next: Option<usize>,
            freeze: bool,
            start: usize,
            len: usize,
        }
        #[derive(PartialEq)]
        struct Pair {
            left: usize,
            right: usize,
            score: f32,
            size: usize,
        }
        impl Eq for Pair {}
        impl Ord for Pair {
            fn cmp(&self, other: &Self) -> Ordering {
                // Highest score first; on a tie, the leftmost pair.
                self.score
                    .partial_cmp(&other.score)
                    .unwrap_or(Ordering::Equal)
                    .then_with(|| other.left.cmp(&self.left))
            }
        }
        impl PartialOrd for Pair {
            fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
                Some(self.cmp(other))
            }
        }

        let mut symbols: Vec<Symbol> = Vec::new();
        let mut rest = 0;
        while rest < normalized.len() {
            let (mblen, freeze) = self.matcher.prefix_match(&normalized[rest..]);
            let index = symbols.len();
            symbols.push(Symbol {
                prev: index.checked_sub(1),
                next: if rest + mblen < normalized.len() { Some(index + 1) } else { None },
                freeze,
                start: rest,
                len: mblen,
            });
            rest += mblen;
        }
        let mut agenda: BinaryHeap<Pair> = BinaryHeap::new();
        let mut rev_merge: HashMap<Vec<u8>, (Vec<u8>, Vec<u8>)> = HashMap::new();
        let maybe_add = |symbols: &Vec<Symbol>,
                         agenda: &mut BinaryHeap<Pair>,
                         rev_merge: &mut HashMap<Vec<u8>, (Vec<u8>, Vec<u8>)>,
                         left: Option<usize>,
                         right: Option<usize>| {
            let (Some(l), Some(r)) = (left, right) else { return };
            if symbols[l].freeze || symbols[r].freeze {
                return;
            }
            let (ls, rs) = (&symbols[l], &symbols[r]);
            let piece = &normalized[ls.start..ls.start + ls.len + rs.len];
            let Some(&id) = self.piece_ids.get(piece) else { return };
            agenda.push(Pair { left: l, right: r, score: self.pieces[id].score, size: piece.len() });
            if self.pieces[id].kind == PieceType::Unused {
                rev_merge.insert(
                    piece.to_vec(),
                    (
                        normalized[ls.start..ls.start + ls.len].to_vec(),
                        normalized[rs.start..rs.start + rs.len].to_vec(),
                    ),
                );
            }
        };
        for i in 1..symbols.len() {
            maybe_add(&symbols, &mut agenda, &mut rev_merge, Some(i - 1), Some(i));
        }
        while let Some(top) = agenda.pop() {
            let (l, r) = (top.left, top.right);
            if symbols[l].len == 0 || symbols[r].len == 0 || symbols[l].len + symbols[r].len != top.size {
                continue;
            }
            symbols[l].len += symbols[r].len;
            symbols[l].next = symbols[r].next;
            if let Some(n) = symbols[r].next {
                symbols[n].prev = Some(l);
            }
            symbols[r].len = 0;
            let prev = symbols[l].prev;
            let next = symbols[l].next;
            maybe_add(&symbols, &mut agenda, &mut rev_merge, prev, Some(l));
            maybe_add(&symbols, &mut agenda, &mut rev_merge, Some(l), next);
        }
        fn resegment(
            sp: &SentencePieceProcessor,
            rev_merge: &HashMap<Vec<u8>, (Vec<u8>, Vec<u8>)>,
            w: &[u8],
            out: &mut Vec<(Vec<u8>, Option<usize>)>,
        ) {
            let id = sp.piece_to_id(w);
            let unused = id.is_some_and(|i| sp.pieces[i].kind == PieceType::Unused);
            if !unused {
                out.push((w.to_vec(), id));
                return;
            }
            match rev_merge.get(w) {
                Some((a, b)) => {
                    resegment(sp, rev_merge, a, out);
                    resegment(sp, rev_merge, b, out);
                }
                None => out.push((w.to_vec(), id)),
            }
        }
        let mut out = Vec::new();
        let mut index = if symbols.is_empty() { None } else { Some(0) };
        while let Some(i) = index {
            let s = &symbols[i];
            resegment(self, &rev_merge, &normalized[s.start..s.start + s.len], &mut out);
            index = s.next;
        }
        out
    }

    // -- char_model.cc Encode --

    fn encode_char(&self, normalized: &[u8]) -> Vec<(Vec<u8>, Option<usize>)> {
        let mut out = Vec::new();
        let mut rest = 0;
        while rest < normalized.len() {
            let (mblen, _) = self.matcher.prefix_match(&normalized[rest..]);
            let w = &normalized[rest..rest + mblen];
            out.push((w.to_vec(), self.piece_to_id(w)));
            rest += mblen;
        }
        out
    }
}

/// The length of the UTF-8 character at the start of `input` (1 for a malformed byte).
fn utf8_len(input: &[u8]) -> usize {
    let n = one_char_len(input[0]).min(input.len());
    if std::str::from_utf8(&input[..n]).is_ok() {
        n
    } else {
        1
    }
}

/// normalizer.cc `DecodePrecompiledCharsMap`: a little-endian u32 trie size, the trie, then the
/// NUL-separated normalized strings.
fn decode_charsmap(blob: &[u8]) -> Result<(Option<DoubleArray>, Vec<u8>)> {
    if blob.is_empty() {
        return Ok((None, Vec::new()));
    }
    if blob.len() <= 4 {
        bail!("Blob for normalization rule is broken.");
    }
    let trie_size = u32::from_le_bytes([blob[0], blob[1], blob[2], blob[3]]) as usize;
    let rest = &blob[4..];
    if trie_size >= rest.len() {
        bail!("Trie data size exceeds the input blob size.");
    }
    let units = rest[..trie_size]
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    Ok((Some(DoubleArray { units }), rest[trie_size..].to_vec()))
}

impl std::fmt::Debug for SentencePieceProcessor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SentencePieceProcessor")
            .field("pieces", &self.pieces.len())
            .field("model_type", &self.model_type)
            .field("normalizer", &self.normalizer.name)
            .finish()
    }
}
