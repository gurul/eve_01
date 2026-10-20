//! The Composio SDK's `to_strict_json_schema` (composio 0.22.0, `composio/utils/strict_schema.py`), which its
//! `OpenAIResponsesProvider(strict=True)` runs over every session tool before the Python bridge sees it.
//!
//! OpenAI's structured outputs need every object node to list all its properties in `required`, set
//! `additionalProperties: false` and carry no annotation-only keywords. Optional properties stay, become required
//! and accept `null`. Constructs strict mode cannot express are reported in `unsupported`; such a tool is sent
//! without strict mode. The input is never changed.

use serde_json::{Map, Value};

pub const MAX_NODE_DEPTH: usize = 512;
pub const MAX_CHANGES: usize = 50;
pub const MAX_REF_CHAIN_DEPTH: usize = 100;

const STRICT_STRIP_KEYWORDS: &[&str] = &["examples", "default"];
const INSTANCE_VALUE_KEYWORDS: &[&str] = &["const", "default", "enum", "examples"];
const SCHEMA_KEYWORDS: &[&str] = &[
    "additionalItems", "additionalProperties", "contains", "contentSchema", "else", "if", "not",
    "propertyNames", "then", "unevaluatedItems", "unevaluatedProperties",
];
const SCHEMA_ARRAY_KEYWORDS: &[&str] = &["allOf", "anyOf", "oneOf", "prefixItems"];
const SCHEMA_MAP_KEYWORDS: &[&str] = &["$defs", "definitions", "dependentSchemas", "patternProperties", "properties"];
const STRICT_UNSUPPORTED_KEYWORDS: &[&str] = &[
    "allOf", "prefixItems", "additionalItems", "contains", "dependencies", "dependentSchemas", "else", "if", "not",
    "propertyNames", "then", "unevaluatedItems", "unevaluatedProperties",
];

#[derive(Debug, Clone, PartialEq)]
pub struct StrictSchemaChange {
    pub path: String,
    pub reason: &'static str,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StrictSchemaIncompatibility {
    pub path: String,
    pub keyword: String,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct StrictJsonSchemaResult {
    /// The strict schema; only usable when `unsupported` is empty.
    pub schema: Value,
    /// The input schema optionality was decided against.
    pub source: Value,
    pub changes: Vec<StrictSchemaChange>,
    pub total_changes: usize,
    pub unsupported: Vec<StrictSchemaIncompatibility>,
}

/// The ValueError the SDK raises for a schema nested deeper than [`MAX_NODE_DEPTH`].
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("JSON Schema exceeds maximum nesting depth of {MAX_NODE_DEPTH}")]
pub struct TooDeep;

fn is_object_type(t: Option<&Value>) -> bool {
    match t {
        Some(Value::String(s)) => s == "object",
        Some(Value::Array(a)) => a.len() == 1 && a[0] == "object",
        _ => false,
    }
}

fn join_path(parent: &str, key: &str) -> String {
    if parent.is_empty() {
        key.to_string()
    } else {
        format!("{parent}.{key}")
    }
}

fn decode_pointer_segment(segment: &str) -> String {
    segment.replace("~1", "/").replace("~0", "~")
}

/// `_try_resolve_pointer`: a local JSON pointer (`#/a/b/0`) against `root`.
pub(crate) fn try_resolve_pointer<'a>(root: &'a Value, pointer: &str) -> Option<&'a Value> {
    if pointer == "#" || pointer.is_empty() {
        return Some(root);
    }
    let rest = pointer.strip_prefix("#/")?;
    let mut cursor = root;
    for raw in rest.split('/') {
        let segment = decode_pointer_segment(raw);
        cursor = match cursor {
            Value::Array(items) => {
                let index: i64 = crate::py_int(&segment)?;
                if index < 0 || index as usize >= items.len() {
                    return None;
                }
                &items[index as usize]
            }
            Value::Object(map) => map.get(&segment)?,
            _ => return None,
        };
    }
    Some(cursor)
}

fn widen_to_nullable(node: &Map<String, Value>) -> Map<String, Value> {
    let null = || Value::Object(Map::from_iter([("type".to_string(), Value::from("null"))]));
    if let Some(Value::Array(any_of)) = node.get("anyOf") {
        if any_of.iter().any(|b| b.get("type") == Some(&Value::from("null"))) {
            return node.clone();
        }
        let mut out = node.clone();
        let mut branches = any_of.clone();
        branches.push(null());
        out.insert("anyOf".into(), Value::Array(branches));
        return out;
    }
    match node.get("type") {
        Some(Value::String(t)) => {
            if t == "null" {
                return node.clone();
            }
            let mut out = node.clone();
            out.insert("type".into(), Value::Array(vec![Value::from(t.as_str()), Value::from("null")]));
            return out;
        }
        Some(Value::Array(types)) => {
            if types.contains(&Value::from("null")) {
                return node.clone();
            }
            let mut out = node.clone();
            let mut t = types.clone();
            t.push(Value::from("null"));
            out.insert("type".into(), Value::Array(t));
            return out;
        }
        _ => {}
    }
    if matches!(node.get("enum"), Some(Value::Array(e)) if e.contains(&Value::Null))
        || node.get("const").is_some_and(Value::is_null)
    {
        return node.clone();
    }
    let annotations: Map<String, Value> = ["description", "title"]
        .iter()
        .filter_map(|k| node.get(*k).map(|v| (k.to_string(), v.clone())))
        .collect();
    let rest: Map<String, Value> =
        node.iter().filter(|(k, _)| !annotations.contains_key(*k)).map(|(k, v)| (k.clone(), v.clone())).collect();
    if rest.is_empty() {
        return node.clone();
    }
    let mut out = annotations;
    out.insert("anyOf".into(), Value::Array(vec![Value::Object(rest), null()]));
    out
}

fn dedupe_required(value: &Value, is_schema: bool, depth: usize) -> Result<Value, TooDeep> {
    if depth > MAX_NODE_DEPTH {
        return Err(TooDeep);
    }
    Ok(match value {
        Value::Array(items) => {
            Value::Array(items.iter().map(|i| dedupe_required(i, is_schema, depth + 1)).collect::<Result<_, _>>()?)
        }
        Value::Object(map) => {
            let mut clone = Map::new();
            for (key, child) in map {
                if is_schema && key == "required" {
                    if let Value::Array(req) = child {
                        let mut seen: Vec<Value> = Vec::new();
                        for r in req {
                            if !seen.contains(r) {
                                seen.push(r.clone());
                            }
                        }
                        clone.insert(key.clone(), Value::Array(seen));
                        continue;
                    }
                }
                let child_schema = is_schema && !INSTANCE_VALUE_KEYWORDS.contains(&key.as_str());
                clone.insert(key.clone(), dedupe_required(child, child_schema, depth + 1)?);
            }
            Value::Object(clone)
        }
        other => other.clone(),
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Value,
    Schema,
    SchemaMap,
    SchemaArray,
}

struct Walker<'a> {
    root: &'a Value,
    changes: Vec<StrictSchemaChange>,
    total_changes: usize,
    unsupported: Vec<StrictSchemaIncompatibility>,
}

impl Walker<'_> {
    fn record(&mut self, path: &str, reason: &'static str, detail: String) {
        self.total_changes += 1;
        if self.changes.len() < MAX_CHANGES {
            self.changes.push(StrictSchemaChange { path: path.to_string(), reason, detail });
        }
    }

    fn reject(&mut self, path: &str, keyword: &str, detail: &str) {
        self.unsupported.push(StrictSchemaIncompatibility {
            path: path.to_string(),
            keyword: keyword.to_string(),
            detail: detail.to_string(),
        });
    }

    fn walk_children(
        &mut self,
        node: &Map<String, Value>,
        mode: Mode,
        depth: usize,
        path: &str,
    ) -> Result<Map<String, Value>, TooDeep> {
        let mut clone = Map::new();
        for (key, child) in node {
            let k = key.as_str();
            let mut child_mode = Mode::Value;
            if mode == Mode::SchemaMap {
                child_mode = Mode::Schema;
            } else if mode == Mode::Schema && !INSTANCE_VALUE_KEYWORDS.contains(&k) {
                if SCHEMA_MAP_KEYWORDS.contains(&k) {
                    child_mode = Mode::SchemaMap;
                } else if SCHEMA_ARRAY_KEYWORDS.contains(&k) || (k == "items" && child.is_array()) {
                    child_mode = Mode::SchemaArray;
                } else if SCHEMA_KEYWORDS.contains(&k) || k == "items" {
                    child_mode = Mode::Schema;
                }
            }
            if child_mode == Mode::SchemaMap {
                if let Value::Object(sub) = child {
                    let mut walked = Map::new();
                    for (name, sub_schema) in sub {
                        let p = join_path(&join_path(path, k), name);
                        walked.insert(name.clone(), self.walk(sub_schema, Mode::Schema, depth + 1, &p)?);
                    }
                    clone.insert(key.clone(), Value::Object(walked));
                    continue;
                }
            }
            let p = join_path(path, k);
            clone.insert(key.clone(), self.walk(child, child_mode, depth + 1, &p)?);
        }
        Ok(clone)
    }

    fn walk(&mut self, value: &Value, mode: Mode, depth: usize, path: &str) -> Result<Value, TooDeep> {
        if depth > MAX_NODE_DEPTH {
            return Err(TooDeep);
        }
        let map = match value {
            Value::Array(items) => {
                let item_mode = if mode == Mode::SchemaArray { Mode::Schema } else { Mode::Value };
                let mut out = Vec::with_capacity(items.len());
                for (index, item) in items.iter().enumerate() {
                    out.push(self.walk(item, item_mode, depth + 1, &format!("{path}[{index}]"))?);
                }
                return Ok(Value::Array(out));
            }
            Value::Object(map) if mode != Mode::Value => map,
            other => return Ok(other.clone()),
        };

        let mut node = Map::new();
        for (key, child) in map {
            if STRICT_STRIP_KEYWORDS.contains(&key.as_str()) {
                self.record(path, "unsupported-keyword-stripped", format!("keyword \"{key}\" removed"));
                continue;
            }
            node.insert(key.clone(), child.clone());
        }
        if node.get("oneOf").is_some_and(Value::is_array) && !node.contains_key("anyOf") {
            let one_of = node.shift_remove("oneOf").unwrap_or(Value::Null);
            node.insert("anyOf".into(), one_of);
            self.record(path, "one-of-converted", "oneOf became anyOf".into());
        }
        for keyword in STRICT_UNSUPPORTED_KEYWORDS {
            if node.contains_key(*keyword) {
                self.reject(path, keyword, &format!("\"{keyword}\" has no strict-mode equivalent"));
            }
        }
        if node.contains_key("oneOf") {
            self.reject(path, "oneOf", "oneOf beside anyOf cannot be merged");
        }
        match node.get("items") {
            Some(Value::Array(_)) => self.reject(path, "items", "tuple-form items has no strict-mode equivalent"),
            Some(Value::Bool(_)) => self.reject(path, "items", "boolean subschema"),
            _ => {}
        }
        if node.get("properties").is_some_and(|p| !p.is_object()) {
            self.reject(path, "properties", "properties is not an object");
        }

        let out = self.walk_children(&node, mode, depth, path)?;

        if let Some(Value::String(r)) = node.get("$ref") {
            let resolved = (r == "#" || r.starts_with("#/")) && try_resolve_pointer(self.root, r).is_some();
            if !resolved {
                self.reject(path, "$ref", &format!("unresolved $ref \"{r}\""));
            }
            // The referenced definition is normalized where it is declared.
            return Ok(Value::Object(out));
        }

        let declares_object = match out.get("type") {
            Some(Value::String(t)) => t == "object",
            Some(Value::Array(t)) => t.contains(&Value::from("object")),
            _ => false,
        };
        if !declares_object && !out.get("properties").is_some_and(Value::is_object) {
            return Ok(Value::Object(out));
        }
        if out.get("properties").is_some_and(|p| !p.is_object()) {
            return Ok(Value::Object(out));
        }

        let mut properties: Map<String, Value> = match out.get("properties") {
            Some(Value::Object(p)) => p.clone(),
            _ => Map::new(),
        };
        let declared_required: Vec<String> = match out.get("required") {
            Some(Value::Array(r)) => r.iter().filter_map(|e| e.as_str().map(str::to_string)).collect(),
            _ => Vec::new(),
        };
        let names: Vec<String> = properties.keys().cloned().collect();
        for name in names {
            let prop_path = join_path(path, &format!("properties.{name}"));
            let widened = match properties.get(&name) {
                Some(Value::Bool(_)) => {
                    self.reject(&prop_path, "properties", "boolean subschema");
                    continue;
                }
                Some(Value::Object(schema)) if !declared_required.contains(&name) => widen_to_nullable(schema),
                _ => continue,
            };
            properties.insert(name.clone(), Value::Object(widened));
            self.record(
                &prop_path,
                "optional-property-nullable",
                format!("property \"{name}\" is now required and accepts null"),
            );
        }

        let additional = out.get("additionalProperties");
        let accepts_dynamic_keys = matches!(additional, Some(Value::Bool(true)) | Some(Value::Object(_)));
        if accepts_dynamic_keys {
            self.reject(path, "additionalProperties", "object accepts arbitrary keys");
        } else if out.contains_key("patternProperties") {
            self.reject(path, "patternProperties", "object accepts pattern-matched keys");
        } else if properties.is_empty() && !out.contains_key("additionalProperties") {
            self.reject(path, "properties", "free-form object accepts arbitrary keys");
        }

        let mut result = out;
        if !result.contains_key("type") {
            result.insert("type".into(), Value::from("object"));
        }
        let required: Vec<Value> = properties.keys().map(|k| Value::from(k.as_str())).collect();
        result.insert("properties".into(), Value::Object(properties));
        result.insert("required".into(), Value::Array(required));
        if !accepts_dynamic_keys {
            result.insert("additionalProperties".into(), Value::Bool(false));
        }
        Ok(Value::Object(result))
    }
}

/// `to_strict_json_schema(schema)`: the strict contract at every depth; `unsupported` non-empty means the
/// returned schema must not be sent as strict.
pub fn to_strict_json_schema(schema: &Value) -> Result<StrictJsonSchemaResult, TooDeep> {
    let empty = Value::Object(Map::new());
    let root = if schema.is_object() { schema } else { &empty };
    let mut walker = Walker { root, changes: Vec::new(), total_changes: 0, unsupported: Vec::new() };
    let walked = walker.walk(root, Mode::Schema, 0, "")?;
    let normalized = dedupe_required(&walked, true, 0)?;
    if !normalized.is_object() || !is_object_type(normalized.get("type")) {
        walker.reject("", "type", "root must be a non-nullable object");
    }
    Ok(StrictJsonSchemaResult {
        schema: normalized,
        source: root.clone(),
        changes: walker.changes,
        total_changes: walker.total_changes,
        unsupported: walker.unsupported,
    })
}
