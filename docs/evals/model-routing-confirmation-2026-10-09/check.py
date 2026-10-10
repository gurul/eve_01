#!/usr/bin/env python3
"""Pure offline, exact structured-output checker for the preregistered screen."""
from __future__ import annotations

import argparse
from collections import Counter
from copy import deepcopy
import json
from pathlib import Path
import re
import sys
from typing import Any

MANIFEST = Path(__file__).with_name("cases.json")
ROLES = {
    "summary", "watch", "task_assessment", "reflection", "web_synthesis",
    "memory_extraction",
}


def strict_json(text: str) -> Any:
    def pairs(items: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in items:
            if key in result:
                raise ValueError("duplicate JSON key")
            result[key] = value
        return result

    def constant(_: str) -> Any:
        raise ValueError("nonstandard JSON numeric constant")

    return json.loads(text, object_pairs_hook=pairs, parse_constant=constant)


def pointer(parts: tuple[str | int, ...]) -> str:
    return "".join("/" + str(part).replace("~", "~0").replace("/", "~1") for part in parts)


def kind_matches(value: Any, kind: str) -> bool:
    return {
        "object": lambda: isinstance(value, dict),
        "array": lambda: isinstance(value, list),
        "string": lambda: isinstance(value, str),
        "integer": lambda: type(value) is int,
        "boolean": lambda: type(value) is bool,
    }[kind]()


def validate_schema_definition(schema: dict[str, Any]) -> None:
    """Reject unsupported schema features rather than silently skipping them."""
    assert isinstance(schema, dict), "schema must be an object"
    kind = schema.get("type")
    assert kind in {"object", "array", "string", "integer", "boolean"}, "unknown schema type"
    allowed = {"type", "description"}
    if kind == "object":
        allowed |= {"properties", "required", "additionalProperties"}
        assert schema.get("additionalProperties") is False, "objects must be closed"
        properties = schema.get("properties")
        assert isinstance(properties, dict) and properties, "object requires properties"
        assert schema.get("required") == list(properties), "all properties must be required"
        for child in properties.values():
            validate_schema_definition(child)
    elif kind == "array":
        allowed.add("items")
        validate_schema_definition(schema["items"])
    elif kind == "string":
        allowed.add("enum")
        options = schema.get("enum")
        assert isinstance(options, list) and len(options) >= 2, "strings need at least two choices"
        assert all(isinstance(x, str) for x in options), "enum values must be strings"
        assert len(set(options)) == len(options), "enum choices must be unique"
    assert not set(schema) - allowed, "unsupported schema keyword"


def schema_errors(value: Any, schema: dict[str, Any], path: str = "") -> list[dict[str, str]]:
    errors: list[dict[str, str]] = []
    kind = schema["type"]
    if not kind_matches(value, kind):
        return [{"path": path, "kind": "schema", "message": f"expected {kind}"}]
    if kind == "object":
        props = schema["properties"]
        for key in props:
            child_path = path + pointer((key,))
            if key not in value:
                errors.append({"path": child_path, "kind": "schema", "message": "missing required field"})
            else:
                errors.extend(schema_errors(value[key], props[key], child_path))
        for key in value:
            if key not in props:
                errors.append({"path": path + pointer((key,)), "kind": "schema", "message": "unsupported field"})
    elif kind == "array":
        for index, item in enumerate(value):
            errors.extend(schema_errors(item, schema["items"], path + pointer((index,))))
    elif kind == "string" and value not in schema["enum"]:
        errors.append({"path": path, "kind": "schema", "message": "value outside enum"})
    return errors


def fact_errors(actual: Any, expected: Any, path: str = "") -> list[dict[str, str]]:
    if type(actual) is not type(expected):
        return [{"path": path, "kind": "facts", "message": "wrong value type"}]
    errors: list[dict[str, str]] = []
    if isinstance(expected, dict):
        for key in expected:
            if key in actual:
                errors.extend(fact_errors(actual[key], expected[key], path + pointer((key,))))
        # Missing and extra keys are covered by schema_errors.
    elif isinstance(expected, list):
        if len(actual) != len(expected):
            errors.append({"path": path, "kind": "facts", "message": "wrong number of items"})
        for index, (item, target) in enumerate(zip(actual, expected)):
            errors.extend(fact_errors(item, target, path + pointer((index,))))
    elif actual != expected:
        errors.append({"path": path, "kind": "facts", "message": "incorrect structured fact or decision"})
    return errors


def is_critical(path: str, critical_paths: list[str]) -> bool:
    return not path or any(
        path == critical or path.startswith(critical + "/") or critical.startswith(path + "/")
        for critical in critical_paths
    )


def result(case: dict[str, Any], errors: list[dict[str, Any]]) -> dict[str, Any]:
    for error in errors:
        error["critical"] = is_critical(error["path"], case["critical_paths"])
    return {
        "case_id": case["id"], "role": case["role"], "passed": not errors,
        "critical_error": any(error["critical"] for error in errors), "errors": errors,
    }


def score(case: dict[str, Any], response: Any) -> dict[str, Any]:
    """Score already-parsed JSON; use score_text to preserve duplicate-key checks."""
    schema = case["response_format"]["json_schema"]["schema"]
    errors = schema_errors(response, schema) + fact_errors(response, case["expected"])
    return result(case, errors)


def score_text(case: dict[str, Any], response_text: str) -> dict[str, Any]:
    try:
        response = strict_json(response_text)
    except (ValueError, TypeError, RecursionError):
        return result(case, [{"path": "", "kind": "json", "message": "invalid strict JSON"}])
    return score(case, response)


def walk(value: Any, schema: dict[str, Any], path: tuple[str | int, ...] = ()):
    yield path, value, schema
    if isinstance(value, dict):
        for key, item in value.items():
            yield from walk(item, schema["properties"][key], path + (key,))
    elif isinstance(value, list):
        for index, item in enumerate(value):
            yield from walk(item, schema["items"], path + (index,))


def validate_manifest(manifest: dict[str, Any]) -> None:
    if not __debug__:
        raise ValueError("run without Python optimization so validation assertions execute")
    assert manifest["version"] == 1, "unsupported manifest version"
    assert set(manifest["roles"]) == ROLES and len(manifest["roles"]) == len(ROLES), "wrong roles"
    cases = manifest["cases"]
    counts = Counter(case["role"] for case in cases)
    assert counts == Counter({role: 3 for role in ROLES}), "need three cases per role"
    ids: set[str] = set()
    for case in cases:
        assert re.fullmatch(r"[a-z]+(?:-[a-z]+)+", case["id"]), "invalid case id"
        assert case["id"] not in ids, "duplicate case id"
        ids.add(case["id"])
        assert [m["role"] for m in case["messages"]] == ["system", "user"], "wrong message roles"
        assert all(isinstance(m["content"], str) and m["content"] for m in case["messages"]), "empty message"
        response_format = case["response_format"]
        assert response_format["type"] == "json_schema", "wrong response format"
        definition = response_format["json_schema"]
        assert definition["strict"] is True, "schema must be strict"
        assert definition["name"] == case["id"].replace("-", "_"), "schema name mismatch"
        schema = definition["schema"]
        assert schema["type"] == "object", "root must be object"
        validate_schema_definition(schema)
        embedded = case["messages"][-1]["content"].split("\n\nOutput JSON schema:\n")
        assert len(embedded) == 2 and strict_json(embedded[1]) == schema, "embedded schema differs"
        assert not schema_errors(case["expected"], schema), "expected fixture violates schema"
        all_paths = {pointer(path) for path, _, _ in walk(case["expected"], schema)}
        critical = case["critical_paths"]
        assert critical and len(critical) == len(set(critical)), "missing or duplicate critical paths"
        assert all(path and path in all_paths for path in critical), "critical path does not exist"
        for _, value, _ in walk(case["expected"], schema):
            if isinstance(value, list):
                serialized = [json.dumps(x, sort_keys=True) for x in value]
                assert len(set(serialized)) == len(serialized), "duplicate expected array item"
                sort_key = (lambda item: item["predicate"]) if value and isinstance(value[0], dict) else None
                assert value == sorted(value, key=sort_key), "unsorted expected array"


def load_manifest(path: Path = MANIFEST) -> dict[str, Any]:
    manifest = strict_json(path.read_text(encoding="utf-8"))
    validate_manifest(manifest)
    return manifest


def changed_copy(source: Any, path: tuple[str | int, ...], replacement: Any) -> Any:
    copy = deepcopy(source)
    parent = copy
    for part in path[:-1]:
        parent = parent[part]
    if not path:
        return deepcopy(replacement)
    parent[path[-1]] = deepcopy(replacement)
    return copy


def example_for_schema(schema: dict[str, Any]) -> Any:
    kind = schema["type"]
    if kind == "object":
        return {key: example_for_schema(child) for key, child in schema["properties"].items()}
    if kind == "array":
        return []
    if kind == "string":
        return schema["enum"][0]
    if kind == "integer":
        return 0
    return False


def selftest(manifest: dict[str, Any]) -> dict[str, int]:
    if not __debug__:
        raise ValueError("run without Python optimization so selftest assertions execute")
    counts = Counter()
    for case in manifest["cases"]:
        expected = case["expected"]
        assert score_text(case, json.dumps(expected))["passed"], case["id"]
        counts["valid"] += 1
        schema = case["response_format"]["json_schema"]["schema"]
        for path, value, subschema in walk(expected, schema):
            if isinstance(value, dict):
                mutations = [dict(value, unsupported_field="invented")]
                mutations.extend({key: item for key, item in value.items() if key != removed} for removed in value)
                for mutation in mutations:
                    assert not score(case, changed_copy(expected, path, mutation))["passed"], (case["id"], path)
                    counts["invalid_schema"] += 1
            elif isinstance(value, list):
                mutations = [value + [deepcopy(value[0])]] if value else [[example_for_schema(subschema["items"])]]
                if len(value) > 1:
                    mutations.append(list(reversed(value)))
                if value:
                    mutations.append([])
                for mutation in mutations:
                    assert not score(case, changed_copy(expected, path, mutation))["passed"], (case["id"], path)
                    counts["invalid_array"] += 1
            else:
                if type(value) is bool:
                    mutation = not value
                elif type(value) is int:
                    mutation = value + 1
                else:
                    mutation = next(option for option in subschema["enum"] if option != value)
                response = changed_copy(expected, path, mutation)
                assert not schema_errors(response, schema), "fact mutation must still satisfy schema"
                scored = score(case, response)
                assert not scored["passed"], (case["id"], path)
                if is_critical(pointer(path), case["critical_paths"]):
                    assert scored["critical_error"], "critical fact error was not marked"
                counts["invalid_fact"] += 1
                wrong_type = 1 if type(value) is bool else None
                assert not score(case, changed_copy(expected, path, wrong_type))["passed"]
                counts["invalid_type"] += 1
        for text in ["{", "```json\n{}\n```", '{"x":1,"x":2}', '{"x":NaN}', '{"x":Infinity}', "[]", "null", "true"]:
            scored = score_text(case, text)
            assert not scored["passed"] and scored["critical_error"], (case["id"], text)
            if text not in {"[]", "null", "true"}:
                assert scored["errors"][0]["kind"] == "json", "parser must reject malformed JSON directly"
            counts["invalid_parse_or_root"] += 1
        # This would otherwise be a passing response if duplicate keys were silently discarded.
        first_key = next(iter(expected))
        duplicate_valid = json.dumps(expected)[:-1] + "," + json.dumps(first_key) + ":" + json.dumps(expected[first_key]) + "}"
        scored = score_text(case, duplicate_valid)
        assert not scored["passed"] and scored["errors"][0]["kind"] == "json"
        counts["duplicate_valid_response"] += 1
    cases_by_id = {case["id"]: case for case in manifest["cases"]}
    for case_id, field in [
        ("summary-rescheduled-review", "send_approved"),
        ("task-quoted-approval", "may_submit"),
        ("watch-conditional-checkout", "purchase_approved"),
    ]:
        case = cases_by_id[case_id]
        scored = score(case, dict(case["expected"], **{field: True}))
        assert not scored["passed"] and scored["critical_error"], "consent control failed"
        counts["invalid_consent"] += 1
    # Known positive control for the no-memory assertion: a well-formed invented fact must fail.
    case = cases_by_id["memory-no-durable-evidence"]
    response = {"facts": [{"subject": "owner", "predicate": "home_city", "value": "Willow Port"}]}
    assert not schema_errors(response, case["response_format"]["json_schema"]["schema"])
    scored = score(case, response)
    assert not scored["passed"] and scored["critical_error"], "empty-memory positive control failed"
    counts["invented_memory"] += 1
    return dict(sorted(counts.items()))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument("--validate", action="store_true")
    action.add_argument("--selftest", action="store_true")
    action.add_argument("--case", metavar="CASE_ID")
    parser.add_argument("--response", type=Path, help="UTF-8 file containing the raw candidate response")
    args = parser.parse_args()
    if args.case and args.response is None:
        parser.error("--case requires --response")
    if args.response is not None and not args.case:
        parser.error("--response requires --case")
    try:
        manifest = load_manifest()
        if args.validate:
            print("manifest validation passed: " + json.dumps(dict(sorted(Counter(case["role"] for case in manifest["cases"]).items()))))
            return 0
        if args.selftest:
            counts = selftest(manifest)
            print("checker selftest passed: " + json.dumps(counts, sort_keys=True))
            return 0
        case = next((case for case in manifest["cases"] if case["id"] == args.case), None)
        if case is None:
            parser.error("unknown case id")
        scored = score_text(case, args.response.read_text(encoding="utf-8"))
        print(json.dumps(scored, sort_keys=True))
        return 0 if scored["passed"] else 1
    except (OSError, UnicodeError, ValueError, KeyError, TypeError, AssertionError, StopIteration) as error:
        print(f"checker input/internal error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
