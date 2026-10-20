//! Python's `plistlib.dumps(value)` (XML format, `sort_keys=True`), byte for byte, for the value types eve's
//! LaunchAgents hold: dicts, arrays, strings, booleans and integers.
//!
//! A hand-written writer rather than the `plist` crate's: launchd reads either, but the owner golden and a plist
//! written by one daemon and compared by the other need Python's exact bytes (tab indents, sorted keys, `<dict/>`
//! for an empty dict, `&gt;` escaped).

/// One plist value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    String(String),
    Bool(bool),
    Integer(i64),
    Array(Vec<Value>),
    /// Keys in insertion order; the writer sorts them as `plistlib` does.
    Dict(Vec<(String, Value)>),
}

impl Value {
    /// A string value.
    pub fn s(text: impl Into<String>) -> Value {
        Value::String(text.into())
    }

    /// Sets `key` in a dict (replacing an existing entry in place, as a Python dict assignment does). No effect on
    /// any other value.
    pub fn set(&mut self, key: &str, value: Value) {
        if let Value::Dict(items) = self {
            match items.iter_mut().find(|(k, _)| k == key) {
                Some(slot) => slot.1 = value,
                None => items.push((key.to_string(), value)),
            }
        }
    }

    /// The value under `key` in a dict, mutably.
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        match self {
            Value::Dict(items) => items.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

/// `plistlib._escape`'s refusal: a string holding a control character other than tab, newline or carriage return.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("strings can't contains control characters; use bytes instead")]
pub struct ControlCharError;

const HEADER: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n";

/// `plistlib.dumps(value)`: the XML plist, as bytes.
pub fn dumps(value: &Value) -> Result<Vec<u8>, ControlCharError> {
    let mut w = Writer { out: String::from(HEADER), level: 0 };
    w.writeln("<plist version=\"1.0\">");
    w.write_value(value)?;
    w.writeln("</plist>");
    Ok(w.out.into_bytes())
}

/// `plistlib._escape`.
fn escape(text: &str) -> Result<String, ControlCharError> {
    if text.chars().any(|c| matches!(c, '\x00'..='\x08' | '\x0b' | '\x0c' | '\x0e'..='\x1f')) {
        return Err(ControlCharError);
    }
    Ok(text
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;"))
}

struct Writer {
    out: String,
    level: usize,
}

impl Writer {
    fn writeln(&mut self, line: &str) {
        if !line.is_empty() {
            for _ in 0..self.level {
                self.out.push('\t');
            }
            self.out.push_str(line);
        }
        self.out.push('\n');
    }

    fn simple(&mut self, element: &str, value: Option<&str>) -> Result<(), ControlCharError> {
        match value {
            Some(v) => {
                let v = escape(v)?;
                self.writeln(&format!("<{element}>{v}</{element}>"));
            }
            None => self.writeln(&format!("<{element}/>")),
        }
        Ok(())
    }

    fn write_value(&mut self, value: &Value) -> Result<(), ControlCharError> {
        match value {
            Value::String(s) => self.simple("string", Some(s)),
            Value::Bool(true) => self.simple("true", None),
            Value::Bool(false) => self.simple("false", None),
            Value::Integer(i) => self.simple("integer", Some(&i.to_string())),
            Value::Array(items) if items.is_empty() => self.simple("array", None),
            Value::Array(items) => {
                self.writeln("<array>");
                self.level += 1;
                for item in items {
                    self.write_value(item)?;
                }
                self.level -= 1;
                self.writeln("</array>");
                Ok(())
            }
            Value::Dict(items) if items.is_empty() => self.simple("dict", None),
            Value::Dict(items) => {
                // sorted(d.items()): str order is code point order, which is UTF-8 byte order.
                let mut sorted: Vec<&(String, Value)> = items.iter().collect();
                sorted.sort_by(|a, b| a.0.cmp(&b.0));
                self.writeln("<dict>");
                self.level += 1;
                for (key, item) in sorted {
                    self.simple("key", Some(key))?;
                    self.write_value(item)?;
                }
                self.level -= 1;
                self.writeln("</dict>");
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_python_plistlib_shapes() {
        let v = Value::Dict(vec![
            ("b".into(), Value::Array(vec![])),
            ("a".into(), Value::Dict(vec![])),
            ("c".into(), Value::Integer(63)),
            ("d".into(), Value::s("x<&>\r\ny\rz")),
        ]);
        let text = String::from_utf8(dumps(&v).unwrap()).unwrap();
        assert_eq!(
            text,
            format!(
                "{HEADER}<plist version=\"1.0\">\n<dict>\n\t<key>a</key>\n\t<dict/>\n\t<key>b</key>\n\t<array/>\n\
                 \t<key>c</key>\n\t<integer>63</integer>\n\t<key>d</key>\n\t<string>x&lt;&amp;&gt;\ny\nz</string>\n\
                 </dict>\n</plist>\n"
            )
        );
    }

    #[test]
    fn a_control_character_is_refused_as_plistlib_does() {
        assert_eq!(dumps(&Value::s("a\x01b")), Err(ControlCharError));
        assert!(dumps(&Value::s("tab\tnewline\n")).is_ok());
        assert_eq!(ControlCharError.to_string(), "strings can't contains control characters; use bytes instead");
    }

    /// The writer against CPython's own plistlib, when a python3 is on PATH: tricky strings, nesting, sorting.
    #[test]
    fn matches_cpython_plistlib_when_python_is_present() {
        let Ok(python) = which::which("python3") else { return };
        let v = Value::Dict(vec![
            ("Zed".into(), Value::s("ünïcødé ✓ & <tag> \"q\" 'a'")),
            ("alpha".into(), Value::Array(vec![Value::Bool(true), Value::Bool(false), Value::Integer(-5)])),
            ("Émile".into(), Value::Dict(vec![("k".into(), Value::s("line\r\nnext\rlast"))])),
            ("_".into(), Value::Dict(vec![])),
        ]);
        let script = r#"
import plistlib, sys
v = {"Zed": "ünïcødé ✓ & <tag> \"q\" 'a'", "alpha": [True, False, -5],
     "Émile": {"k": "line\r\nnext\rlast"}, "_": {}}
sys.stdout.buffer.write(plistlib.dumps(v))
"#;
        let out = std::process::Command::new(python).arg("-I").arg("-c").arg(script).output().unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8(dumps(&v).unwrap()).unwrap(), String::from_utf8(out.stdout).unwrap());
    }
}
