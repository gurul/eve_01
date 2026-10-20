"""Differential cases for eve-ipc's pyjson: json.loads outcomes from CPython."""
import json, random, re, sys, math
rng = random.Random(20261019)
TOKENS = ['{', '}', '[', ']', ',', ':', '"', '"', '"a"', '"k"', '\\', '\\u', 'd800', 'dc00', '00e9', '12', 'zz',
          '0', '1', '7', '9', '-', '+', '.', 'e', 'E', 'null', 'nul', 'true', 'tru', 'false', 'NaN', 'Infinity',
          '-Infinity', ' ', '\n', '\t', '\r', 'x', 'é', '\x01', '\x1f', '/', 'n', 'b', 'f', 't', 'r', '😀',
          '﻿', '12345678901234567890', '1e400', '"\\ud83d\\ude00"', '"\\ud800"', '"\\udc00"']
VALID = ['{"evt": "session_start", "session_id": "abc", "cwd": "/tmp"}', '[1, 2.5, -3e2, true, false, null]',
         '{"a": {"b": [1, {"c": "d\\n\\u00e9"}]}, "e": []}', '"\\ud83d\\ude00 x"', '{"x": -0.0, "y": 1E+2, "z": 0.5e-3}',
         '[[], {}, [[]], {"k": {}}]', '{"evt":"lesson","action":"open","text":"héllo \\"q\\""}', '  42  ', '-0', '"\\/\\b\\f\\r\\t"']

def gen():
    k = rng.random()
    if k < 0.4:
        return ''.join(rng.choice(TOKENS) for _ in range(rng.randint(0, 12)))
    base = rng.choice(VALID)
    if k < 0.6:
        return base[:rng.randint(0, len(base))]
    s = list(base)
    for _ in range(rng.randint(1, 3)):
        op = rng.random()
        i = rng.randint(0, len(s))
        if op < 0.33 and s:
            del s[min(i, len(s) - 1)]
        elif op < 0.66:
            s.insert(i, rng.choice(TOKENS))
        elif s:
            s[min(i, len(s) - 1)] = rng.choice(TOKENS)
    return ''.join(s)

def to_rust(v):
    if isinstance(v, bool) or v is None:
        return v
    if isinstance(v, int):
        if -2**63 <= v < 2**64:
            return v
        return float(v)
    if isinstance(v, float):
        return v
    if isinstance(v, str):
        return re.sub('[\ud800-\udfff]', '�', v)
    if isinstance(v, list):
        return [to_rust(x) for x in v]
    return {to_rust(k): to_rust(x) for k, x in v.items()}

def outcome(s):
    bad = []
    def const(name):
        bad.append({'NaN': 'nan', 'Infinity': 'inf', '-Infinity': '-inf'}[name]); return 0.0
    def flt(text):
        f = float(text)
        if math.isinf(f): bad.append(repr(f))
        return f
    def integer(text):
        i = int(text)
        if not (-2**63 <= i < 2**64) and math.isinf(float(text)): bad.append('inf' if i > 0 else '-inf')
        return i
    try:
        v = json.loads(s, parse_constant=const, parse_float=flt, parse_int=integer)
    except RecursionError as e:
        return {'recursion': str(e)}
    except ValueError as e:
        return {'err': str(e)}
    if bad:
        return {'err': f'Out of range float values are not JSON compliant: {bad[0]}'}
    return {'ok': json.dumps(to_rust(v), ensure_ascii=False)}

seen = set()
cases = []
extra = sys.argv[1:]
for s in extra + [gen() for _ in range(6000)]:
    if s in seen: continue
    seen.add(s)
    o = outcome(s); o['in'] = s
    cases.append(o)
with open('/home/builder/src/eve-00000000000000000000000/rust/crates/eve-ipc/tests/data/pyjson_cases.jsonl', 'w') as f:
    for c in cases:
        f.write(json.dumps(c, ensure_ascii=True) + '\n')
print(len(cases), sum('ok' in c for c in cases), file=sys.stderr)
