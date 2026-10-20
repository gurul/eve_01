//! The part of Python's `difflib` that `match` uses: `get_close_matches` over `SequenceMatcher` (no junk
//! function, `autojunk=True`), on code points, so a close spelling ranks exactly as it does in Python.

use std::collections::HashMap;

/// `difflib.SequenceMatcher` with `b` fixed (as `get_close_matches` sets it once) and `a` set per candidate.
struct Matcher {
    b: Vec<char>,
    /// `b2j`: each character of `b` to the indices where it appears, ascending; popular characters removed.
    b2j: HashMap<char, Vec<usize>>,
    /// `fullbcount`, for `quick_ratio`.
    fullbcount: HashMap<char, usize>,
}

impl Matcher {
    fn new(b: &str) -> Self {
        let b: Vec<char> = b.chars().collect();
        let mut b2j: HashMap<char, Vec<usize>> = HashMap::new();
        for (i, c) in b.iter().enumerate() {
            b2j.entry(*c).or_default().push(i);
        }
        // autojunk: in a sequence of 200 or more, a character that is more than 1% of it is "popular".
        let n = b.len();
        if n >= 200 {
            let ntest = n / 100 + 1;
            b2j.retain(|_, idxs| idxs.len() <= ntest);
        }
        let mut fullbcount = HashMap::new();
        for c in &b {
            *fullbcount.entry(*c).or_insert(0) += 1;
        }
        Matcher { b, b2j, fullbcount }
    }

    fn find_longest_match(&self, a: &[char], alo: usize, ahi: usize, blo: usize, bhi: usize) -> (usize, usize, usize) {
        let b = &self.b;
        let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0usize);
        let mut j2len: HashMap<usize, usize> = HashMap::new();
        for (i, ch) in a.iter().enumerate().take(ahi).skip(alo) {
            let mut newj2len: HashMap<usize, usize> = HashMap::new();
            if let Some(js) = self.b2j.get(ch) {
                for &j in js {
                    if j < blo {
                        continue;
                    }
                    if j >= bhi {
                        break;
                    }
                    let k = j.checked_sub(1).and_then(|p| j2len.get(&p)).copied().unwrap_or(0) + 1;
                    newj2len.insert(j, k);
                    if k > bestsize {
                        besti = i + 1 - k;
                        bestj = j + 1 - k;
                        bestsize = k;
                    }
                }
            }
            j2len = newj2len;
        }
        // No junk function, so nothing is junk: extend over equal (possibly popular) characters on both sides.
        while besti > alo && bestj > blo && a[besti - 1] == b[bestj - 1] {
            besti -= 1;
            bestj -= 1;
            bestsize += 1;
        }
        while besti + bestsize < ahi && bestj + bestsize < bhi && a[besti + bestsize] == b[bestj + bestsize] {
            bestsize += 1;
        }
        (besti, bestj, bestsize)
    }

    /// The total size of `get_matching_blocks()`.
    fn matches(&self, a: &[char]) -> usize {
        let mut queue = vec![(0usize, a.len(), 0usize, self.b.len())];
        let mut total = 0;
        while let Some((alo, ahi, blo, bhi)) = queue.pop() {
            let (i, j, k) = self.find_longest_match(a, alo, ahi, blo, bhi);
            if k > 0 {
                total += k;
                if alo < i && blo < j {
                    queue.push((alo, i, blo, j));
                }
                if i + k < ahi && j + k < bhi {
                    queue.push((i + k, ahi, j + k, bhi));
                }
            }
        }
        total
    }

    fn ratio(&self, a: &[char]) -> f64 {
        calculate_ratio(self.matches(a), a.len() + self.b.len())
    }

    fn quick_ratio(&self, a: &[char]) -> f64 {
        let mut avail: HashMap<char, isize> = HashMap::new();
        let mut matches = 0;
        for c in a {
            let numb = match avail.get(c) {
                Some(n) => *n,
                None => self.fullbcount.get(c).copied().unwrap_or(0) as isize,
            };
            avail.insert(*c, numb - 1);
            if numb > 0 {
                matches += 1;
            }
        }
        calculate_ratio(matches, a.len() + self.b.len())
    }

    fn real_quick_ratio(&self, a: &[char]) -> f64 {
        let (la, lb) = (a.len(), self.b.len());
        calculate_ratio(la.min(lb), la + lb)
    }
}

fn calculate_ratio(matches: usize, length: usize) -> f64 {
    if length > 0 {
        2.0 * matches as f64 / length as f64
    } else {
        1.0
    }
}

/// `difflib.get_close_matches(word, possibilities, n, cutoff)`: the best `n` candidates scoring at least
/// `cutoff`, best first; equal scores rank the later string (by code point) first, as `heapq.nlargest` does.
pub(crate) fn get_close_matches<S: AsRef<str>>(word: &str, possibilities: &[S], n: usize, cutoff: f64) -> Vec<String> {
    let s = Matcher::new(word);
    let mut result: Vec<(f64, &str)> = Vec::new();
    for x in possibilities {
        let x = x.as_ref();
        let a: Vec<char> = x.chars().collect();
        if s.real_quick_ratio(&a) >= cutoff && s.quick_ratio(&a) >= cutoff {
            let r = s.ratio(&a);
            if r >= cutoff {
                result.push((r, x));
            }
        }
    }
    result.sort_by(|l, r| r.0.total_cmp(&l.0).then_with(|| r.1.cmp(l.1)));
    result.into_iter().take(n).map(|(_, x)| x.to_string()).collect()
}

/// `SequenceMatcher(None, a, b).ratio()`, for the parity tests.
#[cfg(test)]
pub(crate) fn ratio(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.chars().collect();
    Matcher::new(b).ratio(&a)
}
