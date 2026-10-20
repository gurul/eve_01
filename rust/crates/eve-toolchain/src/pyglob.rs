//! Python's `glob.glob(pathname)` (recursive=False, include_hidden=False) and `fnmatch.fnmatchcase`, as CPython
//! writes them, so `XCODE_GLOB` finds the same folders in both daemons: a wildcard part lists only folders when more
//! parts follow, skips names starting with "." unless the pattern does, and a literal part only has to exist
//! (`os.path.lexists`, so a dangling link counts).

use eve_profile::pypath;

/// `glob.has_magic`.
fn has_magic(s: &str) -> bool {
    s.contains(['*', '?', '['])
}

fn ishidden(s: &str) -> bool {
    s.starts_with('.')
}

fn lexists(path: &str) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

fn isdir(path: &str) -> bool {
    std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false)
}

/// `glob.glob(pathname)`, in the order the folders list (Python's order too: neither sorts).
pub fn glob(pathname: &str) -> Vec<String> {
    iglob(pathname, false)
}

fn iglob(pathname: &str, dironly: bool) -> Vec<String> {
    let (dirname, basename) = pypath::split(pathname);
    if !has_magic(pathname) {
        let found = if basename.is_empty() { isdir(&dirname) } else { lexists(pathname) };
        return if found { vec![pathname.to_string()] } else { Vec::new() };
    }
    if dirname.is_empty() {
        return glob1("", &basename, dironly);
    }
    let dirs =
        if dirname != pathname && has_magic(&dirname) { iglob(&dirname, true) } else { vec![dirname.clone()] };
    let mut out = Vec::new();
    for d in dirs {
        let names = if has_magic(&basename) { glob1(&d, &basename, dironly) } else { glob0(&d, &basename) };
        out.extend(names.into_iter().map(|name| pypath::join(&d, &name)));
    }
    out
}

/// `glob._glob1`: the names in `dirname` matching `pattern`.
fn glob1(dirname: &str, pattern: &str, dironly: bool) -> Vec<String> {
    let dir = if dirname.is_empty() { "." } else { dirname };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for entry in entries.flatten() {
        // DirEntry.is_dir() follows symlinks in Python's scandir
        if dironly && !std::fs::metadata(entry.path()).map(|m| m.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !ishidden(pattern) && ishidden(&name) {
            continue;
        }
        if fnmatchcase(&name, pattern) {
            names.push(name);
        }
    }
    names
}

/// `glob._glob0`: a literal name, kept when it exists.
fn glob0(dirname: &str, basename: &str) -> Vec<String> {
    if basename.is_empty() {
        if isdir(dirname) {
            return vec![String::new()];
        }
    } else if lexists(&pypath::join(dirname, basename)) {
        return vec![basename.to_string()];
    }
    Vec::new()
}

#[derive(Debug)]
enum Tok {
    Any,
    One,
    Lit(char),
    /// A character set: (negated, items); `None` is the set that matches nothing.
    Set(Option<(bool, Vec<(char, char)>)>),
    AnyOne,
}

/// `fnmatch.translate`, as tokens instead of a regular expression.
fn translate(pat: &[char]) -> Vec<Tok> {
    let n = pat.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        let c = pat[i];
        i += 1;
        match c {
            '*' => {
                if !matches!(out.last(), Some(Tok::Any)) {
                    out.push(Tok::Any);
                }
            }
            '?' => out.push(Tok::One),
            '[' => {
                let mut j = i;
                if j < n && pat[j] == '!' {
                    j += 1;
                }
                if j < n && pat[j] == ']' {
                    j += 1;
                }
                while j < n && pat[j] != ']' {
                    j += 1;
                }
                if j >= n {
                    out.push(Tok::Lit('['));
                    continue;
                }
                let stuff: Vec<char> = pat[i..j].to_vec();
                i = j + 1;
                out.push(charset(&stuff));
            }
            other => out.push(Tok::Lit(other)),
        }
    }
    out
}

/// The set `[stuff]` means in fnmatch: Python splits it into chunks at each "-", drops reversed ranges, and hands
/// the rest to `re`, whose class reads `a-b` as a range and any other "-" as itself.
fn charset(stuff: &[char]) -> Tok {
    // chunks joined by range dashes; inside a chunk every character is literal
    let dashed = stuff.contains(&'-');
    let chunks: Vec<Vec<char>> = if !dashed {
        vec![stuff.to_vec()]
    } else {
        let mut chunks: Vec<Vec<char>> = Vec::new();
        let mut start = 0;
        let mut k = if stuff.first() == Some(&'!') { 2 } else { 1 };
        loop {
            let found = if k < stuff.len() { stuff[k..].iter().position(|&c| c == '-').map(|p| p + k) } else { None };
            let Some(dash) = found else { break };
            chunks.push(stuff[start..dash].to_vec());
            start = dash + 1;
            k = dash + 3;
        }
        let chunk = stuff[start..].to_vec();
        if !chunk.is_empty() {
            chunks.push(chunk);
        } else if let Some(last) = chunks.last_mut() {
            last.push('-');
        }
        // Remove empty ranges: invalid in a regular expression.
        let mut k = chunks.len().saturating_sub(1);
        while k > 0 {
            let (prev_last, next_first) = (chunks[k - 1].last().copied(), chunks[k].first().copied());
            if let (Some(a), Some(b)) = (prev_last, next_first) {
                if a > b {
                    let mut merged = chunks[k - 1][..chunks[k - 1].len() - 1].to_vec();
                    merged.extend_from_slice(&chunks[k][1..]);
                    chunks[k - 1] = merged;
                    chunks.remove(k);
                }
            }
            k -= 1;
        }
        chunks
    };
    // The class text Python builds: "\x" for a literal x inside a chunk, a bare "-" between chunks.
    let mut text: Vec<(char, bool)> = Vec::new(); // (character, escaped)
    for (ci, chunk) in chunks.iter().enumerate() {
        if ci > 0 {
            text.push(('-', false));
        }
        for &c in chunk {
            let escape = matches!(c, '\\' | '&' | '~' | '|') || (c == '-' && dashed);
            text.push((c, escape));
        }
    }
    if text.is_empty() {
        return Tok::Set(None);
    }
    if text.len() == 1 && text[0] == ('!', false) {
        return Tok::AnyOne;
    }
    let mut negated = false;
    let mut first_escaped = false;
    if text[0] == ('!', false) {
        negated = true;
        text.remove(0);
    } else if matches!(text[0].0, '^' | '[') {
        first_escaped = true;
    }
    if first_escaped {
        text[0].1 = true;
    }
    // sre_parse's class: an item, then "-" and another item makes a range; a "-" before "]" is itself
    let mut items = Vec::new();
    let mut p = 0;
    while p < text.len() {
        let lo = text[p].0;
        p += 1;
        if p < text.len() && text[p] == ('-', false) {
            if p + 1 < text.len() {
                let hi = text[p + 1].0;
                items.push((lo, hi));
                p += 2;
                continue;
            }
            items.push((lo, lo));
            items.push(('-', '-'));
            p += 1;
            continue;
        }
        items.push((lo, lo));
    }
    Tok::Set(Some((negated, items)))
}

fn matches_at(toks: &[Tok], name: &[char]) -> bool {
    let Some((tok, rest)) = toks.split_first() else {
        return name.is_empty();
    };
    match tok {
        Tok::Any => (0..=name.len()).any(|skip| matches_at(rest, &name[skip..])),
        _ => {
            let Some((&c, tail)) = name.split_first() else {
                return false;
            };
            let ok = match tok {
                Tok::One | Tok::AnyOne => true,
                Tok::Lit(l) => *l == c,
                Tok::Set(None) => false,
                Tok::Set(Some((negated, items))) => items.iter().any(|&(lo, hi)| lo <= c && c <= hi) != *negated,
                Tok::Any => false,
            };
            ok && matches_at(rest, tail)
        }
    }
}

/// `fnmatch.fnmatchcase(name, pat)`: the whole name matches, case counted.
pub fn fnmatchcase(name: &str, pat: &str) -> bool {
    let pat: Vec<char> = pat.chars().collect();
    let name: Vec<char> = name.chars().collect();
    matches_at(&translate(&pat), &name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnmatchcase_matches_python() {
        // expected values from CPython 3.13 fnmatch.fnmatchcase (eve's venv)
        for (name, pat, want) in [
            ("Xcode.app", "Xcode*.app", true),
            ("Xcode-beta.app", "Xcode*.app", true),
            ("xcode.app", "Xcode*.app", false),
            ("Xcode.apps", "Xcode*.app", false),
            ("abc", "a?c", true),
            ("ac", "a?c", false),
            ("b", "[a-c]", true),
            ("d", "[a-c]", false),
            ("d", "[!a-c]", true),
            ("-", "[a-]", true),
            ("]", "[]]", true),
            ("[", "[", true),
            ("x", "[!]", false),
            ("!", "[!]", false),
            ("[!]", "[!]", true),
            ("b", "[c-a]", false),
            ("-", "[a-b-c]", true),
            ("c", "[a-b-c]", true),
            ("\\", "[\\]", true),
            ("^", "[^]", true),
            ("a\nb", "a*b", true),
            ("d", "[!a-c]", true),
            ("a-", "[a-]", false),
            ("b", "[a-]", false),
            ("-", "[!a-]", false),
            ("z", "[!a-]", true),
            ("b", "[c-a-b]", true),
            ("a", "[c-a-b]", false),
            ("-", "[c-a-b]", true),
            ("x", "[]-a]", false),
            ("^", "[]-a]", true),
            ("&", "[a&&b]", true),
            ("-", "[--z]", true),
            ("m", "[--z]", true),
            ("x", "[!--]", true),
        ] {
            assert_eq!(fnmatchcase(name, pat), want, "{name:?} {pat:?}");
        }
    }

    #[test]
    fn glob_lists_folders_and_skips_hidden() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_string_lossy().into_owned();
        for d in ["Xcode.app/Contents/Developer", "Xcode-beta.app/Contents/Developer", ".Xcode.app/Contents/Developer"]
        {
            std::fs::create_dir_all(format!("{root}/{d}")).unwrap();
        }
        std::fs::create_dir_all(format!("{root}/Xcode-old.app/Contents")).unwrap();
        std::fs::write(format!("{root}/Xcode-file.app"), "").unwrap();
        let mut found = glob(&format!("{root}/Xcode*.app/Contents/Developer"));
        found.sort();
        assert_eq!(
            found,
            vec![
                format!("{root}/Xcode-beta.app/Contents/Developer"),
                format!("{root}/Xcode.app/Contents/Developer"),
            ]
        );
        assert!(glob(&format!("{root}/missing/Xcode*.app/Contents/Developer")).is_empty());
        assert_eq!(glob(&format!("{root}/Xcode.app")), vec![format!("{root}/Xcode.app")]);
        assert!(glob(&format!("{root}/Nope.app")).is_empty());
    }
}
