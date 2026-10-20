//! Python's `string.Template.substitute`, which the scaffold fills its templates with.
//!
//! The pattern is Template's own: `$$` is a dollar, `$identifier` and `${identifier}` are placeholders, where an
//! identifier is `[_a-z][_a-z0-9]*` matched ASCII-only and ignoring case (so `$Name` and `$SWITCH` are whole
//! identifiers, and `$name_set` would be the key `name_set`). Any other `$` is an invalid placeholder.

use std::collections::HashMap;

/// Why `substitute` failed: Python's `KeyError` (a placeholder with no value) or `ValueError` (an invalid one).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TemplateError {
    /// `KeyError(key)`; Python's `str(KeyError('x'))` is `'x'`.
    #[error("{}", eve_app_registry::py_str_repr(.0))]
    Key(String),
    /// `ValueError('Invalid placeholder in string: line N, col M')`.
    #[error("Invalid placeholder in string: line {line}, col {col}")]
    Invalid { line: usize, col: usize },
}

fn id_start(c: char) -> bool {
    c == '_' || c.is_ascii_alphabetic()
}

fn id_rest(c: char) -> bool {
    c == '_' || c.is_ascii_alphanumeric()
}

/// `Template(template).substitute(mapping)`.
pub fn substitute(template: &str, mapping: &HashMap<&str, String>) -> Result<String, TemplateError> {
    let chars: Vec<char> = template.chars().collect();
    let mut out = String::with_capacity(template.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '$' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let next = chars.get(i + 1).copied();
        // escaped: $$
        if next == Some('$') {
            out.push('$');
            i += 2;
            continue;
        }
        // named: $identifier
        if next.is_some_and(id_start) {
            let mut j = i + 2;
            while j < chars.len() && id_rest(chars[j]) {
                j += 1;
            }
            let key: String = chars[i + 1..j].iter().collect();
            out.push_str(mapping.get(key.as_str()).ok_or(TemplateError::Key(key))?);
            i = j;
            continue;
        }
        // braced: ${identifier}
        if next == Some('{') && chars.get(i + 2).copied().is_some_and(id_start) {
            let mut j = i + 3;
            while j < chars.len() && id_rest(chars[j]) {
                j += 1;
            }
            if chars.get(j) == Some(&'}') {
                let key: String = chars[i + 2..j].iter().collect();
                out.push_str(mapping.get(key.as_str()).ok_or(TemplateError::Key(key))?);
                i = j + 1;
                continue;
            }
        }
        // invalid: Template._invalid counts str.splitlines() lines over the text up to and including the `$`
        let (mut line, mut start) = (1, 0);
        let mut k = 0;
        while k < i {
            let c = chars[k];
            if matches!(c, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}') {
                if c == '\r' && k + 1 < i && chars[k + 1] == '\n' {
                    k += 1;
                }
                line += 1;
                start = k + 1;
            }
            k += 1;
        }
        let col = i - start + 1;
        return Err(TemplateError::Invalid { line, col });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words() -> HashMap<&'static str, String> {
        HashMap::from([("name", "fan".to_string()), ("Name", "Fan".to_string()), ("SWITCH", "EVE_FAN".to_string())])
    }

    #[test]
    fn substitutes_as_python_template_does() {
        let w = words();
        assert_eq!(substitute("$Name ${name}_set $SWITCH=1 $$5 a$", &w).unwrap_err(),
                   TemplateError::Invalid { line: 1, col: 34 });
        assert_eq!(substitute("$Name ${name}_set $SWITCH=1 $$5", &w).unwrap(), "Fan fan_set EVE_FAN=1 $5");
        assert_eq!(substitute("$name_set", &w).unwrap_err(), TemplateError::Key("name_set".into()));
        assert_eq!(substitute("$NAME", &w).unwrap_err().to_string(), "'NAME'");
        assert_eq!(substitute("x\ny $1", &w).unwrap_err().to_string(),
                   "Invalid placeholder in string: line 2, col 3");
        assert_eq!(substitute("x\n$", &w).unwrap_err(), TemplateError::Invalid { line: 2, col: 1 });
        assert_eq!(substitute("${name", &w).unwrap_err(), TemplateError::Invalid { line: 1, col: 1 });
        assert_eq!(substitute("${}", &w).unwrap_err(), TemplateError::Invalid { line: 1, col: 1 });
        assert_eq!(substitute("a\r\nb\x0bc$", &w).unwrap_err(), TemplateError::Invalid { line: 3, col: 2 });
        assert_eq!(substitute("é$name.", &w).unwrap(), "éfan.");
    }
}
