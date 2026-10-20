"""Write drop_empty_file_uploads.json: the Composio SDK's own FileHelper.drop_empty_file_uploads answers (what
tools.execute does to a direct tool call's arguments before sending them, auto-upload off) over file-input schemas
shaped like Composio's, with $ref indirection, composed variants and arrays. The Rust tests replay the file.

Run from the repository root with eve's venv:

    bridge/.venv/bin/python rust/crates/eve-composio-tools/tests/fixtures/make_drop_empty_file_uploads.py
"""

from __future__ import annotations

import json
from pathlib import Path
from types import SimpleNamespace

from composio.core.models._files import FileHelper

OUT = Path(__file__).with_name("drop_empty_file_uploads.json")

FILE = {"type": "string", "file_uploadable": True}
FILES = {"type": "array", "items": FILE, "file_uploadable": True}

CASES = [
    # no file input: untouched, "" kept
    ({"type": "object", "properties": {"q": {"type": "string"}}}, {"q": ""}),
    # a plain file input: "" dropped, None and a path kept
    ({"type": "object", "properties": {"f": FILE, "q": {"type": "string"}}}, {"f": "", "q": ""}),
    ({"type": "object", "properties": {"f": FILE}}, {"f": None}),
    ({"type": "object", "properties": {"f": FILE}}, {"f": "/tmp/a.pdf"}),
    # through $ref, with a sibling description, and a dangling ref
    ({"type": "object", "properties": {"f": {"$ref": "#/$defs/F", "description": "the file"}, "g": {"$ref": "#/$defs/Gone"}},
      "$defs": {"F": FILE}}, {"f": "", "g": ""}),
    ({"type": "object", "properties": {"att": {"$ref": "#/definitions/A"}}, "definitions": {"A": {
        "type": "object", "properties": {"name": {"type": "string"}, "data": FILE}}}},
     {"att": {"name": "", "data": ""}}),
    # nested objects and arrays
    ({"type": "object", "properties": {"list": {"type": "array", "items": FILE}}}, {"list": ["", "/a", "", None]}),
    ({"type": "object", "properties": {"list": {"type": "array", "items": [FILE]}}}, {"list": ["", "/b"]}),
    ({"type": "object", "properties": {"files": FILES}}, {"files": ""}),
    ({"type": "object", "properties": {"files": {"type": ["array", "null"], "items": FILE}}}, {"files": None}),
    ({"type": "object", "properties": {"files": {"items": FILE}}}, {"files": ""}),
    # composed: a file variant and a non-file string variant
    ({"type": "object", "properties": {"f": {"anyOf": [FILE, {"type": "null"}]}}}, {"f": ""}),
    ({"type": "object", "properties": {"f": {"anyOf": [{"type": "array", "items": FILE}, {"type": "string"}]}}},
     {"f": ""}),
    ({"type": "object", "properties": {"f": {"anyOf": [{"type": "array", "items": FILE}, {"type": "integer"}]}}},
     {"f": ""}),
    ({"type": "object", "properties": {"f": {"oneOf": [{"type": "object", "properties": {"d": FILE}},
                                                        {"type": "array", "items": FILE}]}}},
     {"f": [""]}),
    ({"type": "object", "properties": {"f": {"allOf": [{"type": "object", "properties": {"d": FILE}}]}}},
     {"f": {"d": "", "e": ""}}),
    # keys the schema does not know are kept as they are
    ({"type": "object", "properties": {"f": FILE}}, {"other": "", "f": ""}),
    # a file flag only inside $defs still triggers the walk
    ({"type": "object", "properties": {"x": {"$ref": "#/$defs/X"}}, "$defs": {"X": {"type": "object", "properties": {
        "y": {"$ref": "#/$defs/Y"}}}, "Y": FILE}}, {"x": {"y": ""}}),
    # a ref cycle is broken, not followed
    ({"type": "object", "properties": {"n": {"$ref": "#/$defs/N"}}, "$defs": {"N": {"type": "object", "properties": {
        "next": {"$ref": "#/$defs/N"}, "f": FILE}}}}, {"n": {"f": "", "next": {"f": "", "next": {}}}}),
]


def main() -> None:
    helper = FileHelper(client=None, outdir="/tmp/composio-fixture-unused")  # type: ignore[arg-type]
    out = []
    for schema, args in CASES:
        tool = SimpleNamespace(input_parameters=schema)
        got = helper.drop_empty_file_uploads(tool=tool, request=json.loads(json.dumps(args)))  # type: ignore[arg-type]
        out.append({"schema": schema, "arguments": args, "sent": got})
    OUT.write_text(json.dumps(out, indent=1) + "\n", encoding="utf-8")
    print(f"{len(out)} cases -> {OUT}")


if __name__ == "__main__":
    main()
