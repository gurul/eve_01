//! `qr_svg`'s QR code, drawn as segno 1.6 draws `segno.make(url, error="m").svg_data_uri(scale=1, border=4,
//! dark="#000", light="#fff", omitsize=True)`: the same version, error level (segno boosts M while the version
//! holds), mask choice, path and percent-encoding, so both daemons hand the page the same data URI.
//!
//! The bit stream and the Reed-Solomon codewords come from the `qrcode` crate (`Bits`, `construct_codewords`),
//! which writes them as segno does; the matrix, the mask evaluation and the format and version information are
//! segno's own code (encoder.py), ported line for line, because `qrcode`'s matrix and mask choice differ from
//! segno's.

use qrcode::bits::Bits;
use qrcode::ec::construct_codewords;
use qrcode::types::{EcLevel, Version};

/// segno's `consts.FORMAT_INFO`: M masks 0-7, then L, H, Q.
const FORMAT_INFO: [u32; 32] = [
    0x5412, 0x5125, 0x5e7c, 0x5b4b, 0x45f9, 0x40ce, 0x4f97, 0x4aa0, // M
    0x77c4, 0x72f3, 0x7daa, 0x789d, 0x662f, 0x6318, 0x6c41, 0x6976, // L
    0x1689, 0x13be, 0x1ce7, 0x19d0, 0x0762, 0x0255, 0x0d0c, 0x083b, // H
    0x355f, 0x3068, 0x3f31, 0x3a06, 0x24b4, 0x2183, 0x2eda, 0x2bed, // Q
];

/// segno's `consts.VERSION_INFO`: versions 7 to 40.
const VERSION_INFO: [u32; 34] = [
    0x07c94, 0x085bc, 0x09a99, 0x0a4d3, 0x0bbf6, 0x0c762, 0x0d847, 0x0e60d, 0x0f928, 0x10b78, 0x1145d, 0x12a17,
    0x13532, 0x149a6, 0x15683, 0x168c9, 0x177ec, 0x18ec4, 0x191e1, 0x1afab, 0x1b08e, 0x1cc1a, 0x1d33f, 0x1ed75,
    0x1f250, 0x209d5, 0x216f0, 0x228ba, 0x2379f, 0x24b0b, 0x2542e, 0x26a64, 0x27541, 0x28c69,
];

/// segno's `consts.ALIGNMENT_POS`: version 2 onwards.
const ALIGNMENT_POS: [&[usize]; 39] = [
    &[6, 18], &[6, 22], &[6, 26], &[6, 30], &[6, 34],
    &[6, 22, 38], &[6, 24, 42], &[6, 26, 46], &[6, 28, 50], &[6, 30, 54], &[6, 32, 58], &[6, 34, 62],
    &[6, 26, 46, 66], &[6, 26, 48, 70], &[6, 26, 50, 74], &[6, 30, 54, 78], &[6, 30, 56, 82], &[6, 30, 58, 86],
    &[6, 34, 62, 90],
    &[6, 28, 50, 72, 94], &[6, 26, 50, 74, 98], &[6, 30, 54, 78, 102], &[6, 28, 54, 80, 106],
    &[6, 32, 58, 84, 110], &[6, 30, 58, 86, 114], &[6, 34, 62, 90, 118],
    &[6, 26, 50, 74, 98, 122], &[6, 30, 54, 78, 102, 126], &[6, 26, 52, 78, 104, 130],
    &[6, 30, 56, 82, 108, 134], &[6, 34, 60, 86, 112, 138], &[6, 30, 58, 86, 114, 142],
    &[6, 34, 62, 90, 118, 146],
    &[6, 30, 54, 78, 102, 126, 150], &[6, 24, 50, 76, 102, 128, 154], &[6, 28, 54, 80, 106, 132, 158],
    &[6, 32, 58, 84, 110, 136, 162], &[6, 26, 54, 82, 110, 138, 166], &[6, 30, 58, 86, 114, 142, 170],
];

/// segno's `_FINDER_PATTERN`: the finder with its separator on every side.
const FINDER: [[u8; 9]; 9] = [
    [0, 0, 0, 0, 0, 0, 0, 0, 0],
    [0, 1, 1, 1, 1, 1, 1, 1, 0],
    [0, 1, 0, 0, 0, 0, 0, 1, 0],
    [0, 1, 0, 1, 1, 1, 0, 1, 0],
    [0, 1, 0, 1, 1, 1, 0, 1, 0],
    [0, 1, 0, 1, 1, 1, 0, 1, 0],
    [0, 1, 0, 0, 0, 0, 0, 1, 0],
    [0, 1, 1, 1, 1, 1, 1, 1, 0],
    [0, 0, 0, 0, 0, 0, 0, 0, 0],
];

/// "no value yet": segno's illegal 0x2, the encoding region.
const FREE: u8 = 0x2;

type Matrix = Vec<Vec<u8>>;

/// The segment's mode, as segno's `find_mode` picks one for a string (Kanji aside: see PARITY.md).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Numeric,
    Alphanumeric,
    Byte,
}

const ALNUM: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ $%*+-./:";

fn find_mode(data: &[u8]) -> Mode {
    if !data.is_empty() && data.iter().all(u8::is_ascii_digit) {
        Mode::Numeric
    } else if !data.is_empty() && data.iter().all(|b| ALNUM.contains(b)) {
        Mode::Alphanumeric
    } else {
        Mode::Byte
    }
}

/// segno's `data_to_bytes` for a str: ISO-8859-1 when every character fits, else UTF-8.
fn data_to_bytes(text: &str) -> Vec<u8> {
    if text.chars().all(|c| (c as u32) <= 0xFF) {
        text.chars().map(|c| c as u32 as u8).collect()
    } else {
        text.as_bytes().to_vec()
    }
}

/// The segment's bits at `version` (mode, count, data), and the data capacity in bits at `ec`; None when they do
/// not fit (segno's `find_version`: the bit length with its overhead within the capacity).
fn segment(data: &[u8], mode: Mode, version: i16, ec: EcLevel) -> Option<(Bits, usize)> {
    let mut bits = Bits::new(Version::Normal(version));
    match mode {
        Mode::Numeric => bits.push_numeric_data(data).ok()?,
        Mode::Alphanumeric => bits.push_alphanumeric_data(data).ok()?,
        Mode::Byte => bits.push_byte_data(data).ok()?,
    }
    let capacity = bits.max_len(ec).ok()?;
    (bits.len() <= capacity).then_some((bits, capacity))
}

/// The data codewords as segno's `_encode` writes them: the terminator (at most 4 bits), then
/// `write_padding_bits`, which adds `8 - length % 8` zero bits (a whole zero byte when the stream already ends on
/// a byte), then the pad codewords 0xEC 0x11 alternately; `make_blocks` takes the capacity's codewords from it.
fn data_codewords(bits: Bits, capacity: usize) -> Vec<u8> {
    let length = bits.len();
    let length = length + (capacity - length).min(4);
    let length = length + (8 - length % 8);
    let mut out = bits.into_bytes();
    out.resize(length / 8, 0);
    let pads = (capacity / 8).saturating_sub(length / 8);
    out.extend((0..pads).map(|i| if i % 2 == 0 { 0xEC } else { 0x11 }));
    out.truncate(capacity / 8);
    out
}

fn ec_index(ec: EcLevel) -> usize {
    // FORMAT_INFO's blocks: M, L, H, Q (segno adds 0x08 for L, 0x10 for H, 0x18 for Q)
    match ec {
        EcLevel::M => 0,
        EcLevel::L => 8,
        EcLevel::H => 16,
        _ => 24,
    }
}

/// segno's `make_matrix(width, height)`: 0x2 everywhere, the version and format areas 0, and the timing pattern.
fn make_matrix(width: usize) -> Matrix {
    let mut m = vec![vec![FREE; width]; width];
    if width > 41 {
        for i in 0..6 {
            m[i][width - 11] = 0;
            m[i][width - 10] = 0;
            m[i][width - 9] = 0;
            m[width - 11][i] = 0;
            m[width - 10][i] = 0;
            m[width - 9][i] = 0;
        }
    }
    for i in 0..9 {
        m[i][8] = 0;
        m[8][i] = 0;
        // matrix[-i][8] and row_eight[-i]: i = 0 is row (or column) 0 again
        m[(width - i) % width][8] = 0;
        m[8][(width - i) % width] = 0;
    }
    let mut bit = 1u8;
    for i in 8..width - 8 {
        m[i][6] = bit;
        m[6][i] = bit;
        bit ^= 1;
    }
    m
}

fn add_finder_patterns(m: &mut Matrix) {
    let width = m.len();
    // upper left, upper right, bottom left: (row, col)
    for (i, j, top, left) in [(0usize, 0usize, true, true), (0, width - 8, true, false), (width - 8, 0, false, true)] {
        let offset = usize::from(top);
        let sepoffset = usize::from(left);
        for r in 0..8 {
            m[i + r][j..j + 8].copy_from_slice(&FINDER[offset + r][sepoffset..sepoffset + 8]);
        }
    }
}

fn add_alignment_patterns(m: &mut Matrix) {
    let width = m.len();
    let version = (width - 17) / 4;
    if version < 2 {
        return;
    }
    const PATTERN: [[u8; 5]; 5] =
        [[1, 1, 1, 1, 1], [1, 0, 0, 0, 1], [1, 0, 1, 0, 1], [1, 0, 0, 0, 1], [1, 1, 1, 1, 1]];
    let positions = ALIGNMENT_POS[version - 2];
    let (lo, hi) = (positions[0], positions[positions.len() - 1]);
    for &x in positions {
        for &y in positions {
            if (x, y) == (lo, lo) || (x, y) == (lo, hi) || (x, y) == (hi, lo) {
                continue;
            }
            for (r, row) in PATTERN.iter().enumerate() {
                m[x - 2 + r][y - 2..y + 3].copy_from_slice(row);
            }
        }
    }
}

/// segno's `add_codewords`; the remainder bits (zeros) fill what is left, as `make_final_message` appends them.
fn add_codewords(m: &mut Matrix, bits: &[u8]) {
    let size = m.len();
    let mut idx = 0;
    let mut right = size - 1;
    loop {
        let r = if right <= 6 { right - 1 } else { right };
        for vertical in 0..size {
            for z in 0..2 {
                let j = r - z;
                let upwards = ((r & 2) == 0) ^ (j < 6);
                let i = if upwards { size - 1 - vertical } else { vertical };
                if m[i][j] == FREE {
                    m[i][j] = bits.get(idx).copied().unwrap_or(0);
                    idx += 1;
                }
            }
        }
        if right < 3 {
            break;
        }
        right -= 2;
    }
}

fn mask_bit(mask: usize, i: usize, j: usize) -> bool {
    match mask {
        0 => (i + j) & 1 == 0,
        1 => i & 1 == 0,
        2 => j.is_multiple_of(3),
        3 => (i + j).is_multiple_of(3),
        4 => (i / 2 + j / 3) & 1 == 0,
        5 => {
            let t = i * j;
            (t & 1) + (t % 3) == 0
        }
        6 => {
            let t = i * j;
            ((t & 1) + (t % 3)) & 1 == 0
        }
        _ => (((i + j) & 1) + (i * j) % 3) & 1 == 0,
    }
}

/// segno's `mask_scores`, summed (`evaluate_mask`).
fn evaluate_mask(m: &Matrix) -> u64 {
    let size = m.len();
    let n3 = [1u8, 0, 1, 1, 1, 0, 1];
    let occurrences = |seq: &[u8]| -> u64 {
        let mut count = 0;
        let find = |from: usize| -> Option<usize> {
            (from..=seq.len().saturating_sub(7)).find(|&k| k + 7 <= seq.len() && seq[k..k + 7] == n3)
        };
        let mut idx = find(0);
        while let Some(at) = idx {
            let mut offset = at + 7;
            let before = &seq[at.saturating_sub(4)..at.min(size)];
            let after = &seq[offset.min(size)..(offset + 4).min(size)];
            if at == 0 || at + 7 == size || !before.iter().any(|b| *b != 0) || !after.iter().any(|b| *b != 0) {
                count += 40;
            } else {
                offset = at + 4;
            }
            idx = find(offset);
        }
        count
    };
    let (mut n1, mut n2, mut n3_score) = (0u64, 0u64, 0u64);
    let mut dark = 0u64;
    let mut column = vec![0u8; size];
    for i in 0..size {
        let row = &m[i];
        let (mut row_prev, mut col_prev) = (-1i16, -1i16);
        let (mut row_run, mut col_run) = (0u64, 0u64);
        for j in 0..size {
            let rc = row[j];
            let cc = m[j][i];
            column[j] = cc;
            dark += u64::from(rc);
            if i16::from(rc) == row_prev {
                row_run += 1;
            } else {
                if row_run >= 5 {
                    n1 += row_run - 2;
                }
                row_run = 1;
            }
            if i16::from(cc) == col_prev {
                col_run += 1;
            } else {
                if col_run >= 5 {
                    n1 += col_run - 2;
                }
                col_run = 1;
            }
            if i > 0 && j > 0 {
                let last = &m[i - 1];
                if i16::from(rc) == row_prev && rc == last[j] && last[j] == last[j - 1] {
                    n2 += 3;
                }
            }
            row_prev = i16::from(rc);
            col_prev = i16::from(cc);
        }
        n3_score += occurrences(row);
        n3_score += occurrences(&column);
        if row_run >= 5 {
            n1 += row_run - 2;
        }
        if col_run >= 5 {
            n1 += col_run - 2;
        }
    }
    let percent = dark as f64 / (size * size) as f64;
    let n4 = 10 * ((percent * 100.0 - 50.0).abs() / 5.0) as u64;
    n1 + n2 + n3_score + n4
}

fn add_format_info(m: &mut Matrix, ec: EcLevel, mask: usize) {
    let size = m.len();
    let info = FORMAT_INFO[ec_index(ec) + mask];
    let (mut voffset, mut hoffset) = (0usize, 0usize);
    for i in 0..8 {
        let vbit = ((info >> i) & 1) as u8;
        let hbit = ((info >> (14 - i)) & 1) as u8;
        if i == 6 {
            voffset += 1;
            hoffset = 1;
        }
        m[i + voffset][8] = vbit;
        m[8][i + hoffset] = hbit;
        m[8][size - 1 - i] = vbit;
        m[size - 1 - i][8] = hbit;
    }
    m[size - 8][8] = 1;
}

fn add_version_info(m: &mut Matrix, version: usize) {
    if version < 7 {
        return;
    }
    let size = m.len();
    let info = VERSION_INFO[version - 7];
    for i in 0..6 {
        let b1 = ((info >> (i * 3)) & 1) as u8;
        let b2 = ((info >> (i * 3 + 1)) & 1) as u8;
        let b3 = ((info >> (i * 3 + 2)) & 1) as u8;
        m[size - 11][i] = b1;
        m[size - 10][i] = b2;
        m[size - 9][i] = b3;
        m[i][size - 11] = b1;
        m[i][size - 10] = b2;
        m[i][size - 9] = b3;
    }
}

/// The QR code's modules (1 dark), rows top to bottom, with its version, error level and mask, as
/// `segno.make(text, error="m")` makes them. None when the text does not fit version 40.
pub(crate) fn matrix(text: &str) -> Option<(Matrix, usize, EcLevel, usize)> {
    let data = data_to_bytes(text);
    let mode = find_mode(&data);
    let version = (1..=40).find(|v| segment(&data, mode, *v, EcLevel::M).is_some())?;
    // boost_error_level: M, then Q, then H while the version holds
    let mut ec = EcLevel::M;
    for better in [EcLevel::Q, EcLevel::H] {
        if segment(&data, mode, version, better).is_none() {
            break;
        }
        ec = better;
    }
    let (bits, capacity) = segment(&data, mode, version, ec)?;
    let raw = data_codewords(bits, capacity);
    let (data_cw, ec_cw) = construct_codewords(&raw, Version::Normal(version), ec).ok()?;
    let stream: Vec<u8> =
        data_cw.iter().chain(ec_cw.iter()).flat_map(|b| (0..8).rev().map(move |k| (b >> k) & 1)).collect();
    let width = 17 + 4 * version as usize;
    let mut m = make_matrix(width);
    add_finder_patterns(&mut m);
    add_alignment_patterns(&mut m);
    add_codewords(&mut m, &stream);
    // find_and_apply_best_mask: the function patterns, the format areas and the dark module are not data
    let mut function = make_matrix(width);
    add_finder_patterns(&mut function);
    add_alignment_patterns(&mut function);
    function[width - 8][8] = 1;
    let mut best: Option<(u64, usize, Matrix)> = None;
    for mask in 0..8 {
        let mut candidate = m.clone();
        for i in 0..width {
            for j in 0..width {
                if function[i][j] > 1 && mask_bit(mask, i, j) {
                    candidate[i][j] ^= 1;
                }
            }
        }
        let score = evaluate_mask(&candidate);
        if best.as_ref().is_none_or(|(s, _, _)| score < *s) {
            best = Some((score, mask, candidate));
        }
    }
    let (_, mask, mut m) = best?;
    add_format_info(&mut m, ec, mask);
    add_version_info(&mut m, version as usize);
    Some((m, version as usize, ec, mask))
}

/// segno's `utils.matrix_to_lines`, then `write_svg`'s relative path: "M4 4.5h7m1 0h4...".
fn dark_path(m: &Matrix, border: usize) -> String {
    let mut out = String::new();
    // matrix_to_lines yields ((x1, y), (x2, y)); y carries the half module
    let mut lines: Vec<(usize, usize, f64)> = Vec::new();
    let mut y = border as f64 + 0.5 - 1.0;
    let mut last = 1u8;
    for row in m {
        let (mut x1, mut x2) = (border, border);
        y += 1.0;
        for &bit in row {
            if last != bit && bit == 0 {
                lines.push((x1, x2, y));
                x1 = x2;
            }
            x2 += 1;
            if bit == 0 {
                x1 += 1;
            }
            last = bit;
        }
        if last != 0 {
            lines.push((x1, x2, y));
            last = 0;
        }
    }
    let (mut px, mut py) = (0i64, 0f64);
    for (i, (x1, x2, y1)) in lines.into_iter().enumerate() {
        let dx = x1 as i64 - px;
        let dy = y1 - py;
        let dy_text = if dy.fract() == 0.0 { format!("{}", dy as i64) } else { format!("{dy}") };
        out.push_str(&format!("{}{} {}h{}", if i > 0 { 'm' } else { 'M' }, dx, dy_text, x2 - x1));
        px = x2 as i64;
        py = y1;
    }
    out
}

/// Python's `urllib.parse.quote(text, safe=b"")`.
fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `segno.make(text, error="m").svg_data_uri(scale=1, border=4, dark="#000", light="#fff", omitsize=True)`;
/// None when the text does not fit a QR code.
pub(crate) fn svg_data_uri(text: &str) -> Option<String> {
    let (m, _, _, _) = matrix(text)?;
    let border = 4;
    let size = m.len() + 2 * border;
    // the background path ("M0 0h<w>v<h>h-<w>z") is the shorter one, so it comes first (sorted by length)
    let background = format!("<path fill=\"#fff\" d=\"M0 0h{size}v{size}h-{size}z\"/>");
    let dark = format!("<path class=\"qrline\" stroke=\"#000\" d=\"{}\"/>", dark_path(&m, border));
    let mut paths = [background, dark];
    paths.sort_by_key(String::len);
    let svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {size} {size}\" class=\"segno\">{}</svg>",
        paths.concat()
    );
    // _replace_quotes: (=)"([^"]+)" becomes \1'\2'
    let quoted = REPLACE_QUOTES.replace_all(&svg, "='$1'");
    Some(format!("data:image/svg+xml;charset=utf-8,{}", quote(&quoted)))
}

static REPLACE_QUOTES: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r#"="([^"]+)""#).expect("REPLACE_QUOTES"));
