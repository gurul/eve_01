"""Write strict_schemas.json: the Composio SDK's own to_strict_json_schema answers over schemas shaped like the
ones Composio serves, plus edge cases. The Rust tests replay the file and must agree with every field.

Run from the repository root with eve's venv:

    bridge/.venv/bin/python rust/crates/eve-composio-tools/tests/fixtures/make_strict_schemas.py
"""

from __future__ import annotations

import json
from pathlib import Path

from composio.utils.strict_schema import to_strict_json_schema

OUT = Path(__file__).with_name("strict_schemas.json")

S = "string"
CASES = [
    {"type": "object", "properties": {"x": {"$ref": "#/$defs/X"}, "y": {"$ref": "#/$defs/Missing"}},
     "$defs": {"X": {"type": "string"}}},
    {"type": "object", "properties": {}, "additionalProperties": True},
    {"type": "object"},
    [1],
    {"type": "object", "properties": {"a": {"allOf": []}}},
    {"type": "object", "properties": {"a": {"allOf": [{"type": S}]}}, "required": ["a"]},
    {"type": "object", "properties": {
        "a": {"type": S, "default": "x"}, "b": {"type": "integer"},
        "c": {"oneOf": [{"type": S}, {"type": "integer"}], "description": "either"},
        "d": {"enum": ["x", None]}, "e": {"type": "object", "properties": {"f": {"type": "boolean"}}, "required": ["f"]},
        "g": {"description": "free", "minimum": 1}}, "required": ["b", "b"]},
    {"type": "object", "properties": {"t": {"type": ["string", "integer"]}, "n": {"type": "null"},
                                       "u": {"type": ["string", "null"]}, "c": {"const": 3}, "k": {"const": None}}},
    {"type": "object", "properties": {"items": {"type": "array", "items": {"type": "object", "properties":
        {"id": {"type": S}}}}, "tup": {"type": "array", "items": [{"type": S}, {"type": "integer"}]}},
     "required": ["items"]},
    {"type": "object", "properties": {"m": {"type": "object", "additionalProperties": {"type": S}}},
     "required": ["m"]},
    {"type": "object", "properties": {"m": {"type": "object", "patternProperties": {"^x": {"type": S}}}},
     "required": ["m"]},
    {"type": "object", "properties": {"q": {"type": S, "format": "email", "pattern": "^a", "minLength": 1,
                                            "title": "Q", "examples": ["a"]}}, "required": ["q"]},
    {"type": "object", "properties": {"r": {"$ref": "#/definitions/R", "description": "with a sibling"}},
     "definitions": {"R": {"type": "object", "properties": {"z": {"type": S}}}}, "required": ["r"]},
    {"type": "object", "properties": {"x": {"not": {"type": S}}, "y": {"if": {}, "then": {}}}, "required": ["x", "y"]},
    {"type": ["object", "null"], "properties": {}},
    {"type": "object", "properties": {"a": True, "b": False}},
    {"type": "object", "properties": {"a": {"type": S}}, "required": ["a", "missing"]},
    {"type": "object", "properties": {"a": {"anyOf": [{"type": S}, {"type": "null"}]}}},
    {"type": "object", "properties": {"a": {"type": "object", "properties": {}, "additionalProperties": False}},
     "required": ["a"]},
    {"type": "object", "properties": {"deep": {"type": "object", "properties": {"deeper": {"type": "object",
        "properties": {"leaf": {"type": "integer", "maximum": 9}}}}}}},
    {"type": "object", "properties": {"x": {"$ref": "#/$defs/A"}}, "$defs": {"A": {"$ref": "#/$defs/B"},
                                                                             "B": {"type": S}}, "required": ["x"]},
    {"type": "object", "properties": {"x": {"$ref": "http://elsewhere/x.json"}}, "required": ["x"]},
    {"type": "object", "properties": {"x": {"$ref": "#/$defs/a~1b"}}, "$defs": {"a/b": {"type": S}},
     "required": ["x"]},
    {"type": "object", "properties": {"x": {"$ref": "#/$defs/L/0"}}, "$defs": {"L": [{"type": S}]},
     "required": ["x"]},
    {"type": "object", "properties": {f"p{i}": {"type": S} for i in range(60)}},
    {"properties": {"a": {"type": S}}},
    {"type": "object", "properties": {"a": {"type": S}}, "required": "a"},
    {"type": "object", "properties": {"a": {"type": S}}, "additionalProperties": {"type": S}},
    {},
    "string",
]


def row(schema):
    r = to_strict_json_schema(schema)
    return {"input": schema, "schema": r.schema, "source": r.source, "total_changes": r.total_changes,
            "changes": [[c.path, c.reason, c.detail] for c in r.changes],
            "unsupported": [[u.path, u.keyword, u.detail] for u in r.unsupported]}


rows = []
for s in CASES:
    try:
        rows.append(row(s))
    except Exception as e:  # noqa: BLE001
        rows.append({"input": s, "error": type(e).__name__})
OUT.write_text(json.dumps(rows, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
print(f"{len(rows)} cases -> {OUT}")
