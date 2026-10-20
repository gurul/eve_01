# Regenerates python_cases.json from the Python module (run from bridge/ with the daemon's Python 3.13):
#   python -I ../rust/crates/eve-jev-verify/tests/fixtures/gen_cases.py ../rust/crates/eve-jev-verify/tests/fixtures/python_cases.json
import json, os, random, sys
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), *[".."] * 5, "bridge", "src"))
from eve_bridge import jev_verify as jv

rng = random.Random(20261019)
ALPH = list("abcAB Zz  ß∑σΣ0123456789٣²½,.,.$€-:%_/\n\t·🔥éİﬁ") + ["\u0301", "\r\n", "\u2028", "\x1c", "\u00a0", "Ⅻ", "一"]
def s(n=None):
    n = n if n is not None else rng.randint(0, 24)
    return "".join(rng.choice(ALPH) for _ in range(n))
cases = {"tokens": [], "shown": [], "relevant": [], "lines": [], "option": [], "parse": [], "extract": [], "render": []}
for _ in range(400):
    t = s(40)
    cases["tokens"].append({"text": t, "tokens": jv._tokens(t), "numbers": sorted(jv._numbers(t)), "words": sorted(jv._words(t))})
for _ in range(400):
    e = s(); lines = tuple(s() for _ in range(rng.randint(0, 5)))
    cases["shown"].append({"expect": e, "lines": list(lines), "numbers": jv.numbers_shown(e, lines), "words": jv.words_shown(e, lines)})
for _ in range(200):
    lines = tuple(s(8) for _ in range(rng.randint(0, 50)))
    ex = tuple(s(10) for _ in range(rng.randint(0, 3)))
    lim = rng.choice([3, 5, 30])
    cases["relevant"].append({"lines": list(lines), "expects": list(ex), "limit": lim, "got": list(jv.relevant_lines(lines, ex, lim))})
for _ in range(30):
    t = s(rng.randint(0, 1500))
    cases["lines"].append({"text": t, "lines": list(jv.screen_lines(t))})
for _ in range(300):
    opts = tuple(s(6) for _ in range(rng.randint(0, 4)))
    v = rng.choice([s(4), opts[0] if opts else s(2), (opts[0][:2] if opts else "")])
    c = jv.Control("0", "select", "S", options=opts)
    cases["option"].append({"options": list(opts), "value": v, "got": jv._option_for(c, v)})
def rv():
    return rng.choice([None, True, False, 0, 1, 2.5, 1e20, 1e-7, -0.0, 30.0, "", "  x  y ", s(), "tap", "TYPE", "Choose", ["l"], {"k": 1}])
for _ in range(300):
    raw = [ {k: rv() for k in rng.sample(["name", "steps", "expect", "x"], rng.randint(0, 4))} for _ in range(rng.randint(0, 6))]
    for j in raw:
        if "steps" in j and rng.random() < 0.8:
            j["steps"] = [{"do": rng.choice(["tap", "type", "choose", "TAP", "swipe", rv()]), "target": rv(), "value": rv()} if rng.random() < 0.9 else rv() for _ in range(rng.randint(0, 14))]
        if "expect" in j and rng.random() < 0.8:
            j["expect"] = [rv() for _ in range(rng.randint(0, 7))]
    if rng.random() < 0.1:
        raw = rv()
    found, problems = jv.parse_journeys(raw)
    cases["parse"].append({"raw": raw, "found": jv.journeys_json(found), "problems": problems})
def good():
    return rng.choice([s(), s(100), s(200), "  Add \n person ", 30, 2.5, 1e16, 1e-5, "x"])
for _ in range(150):
    raw = [{"name": rng.choice([good(), None, ""]),
            "steps": [{"do": rng.choice(["tap", "type", "choose", "Tap", " type "]), "target": good(), "value": rng.choice([good(), good(), ""])}
                      for _ in range(rng.randint(1, 14))],
            "expect": [good() for _ in range(rng.randint(0, 7))]} for _ in range(rng.randint(1, 6))]
    found, problems = jv.parse_journeys(raw)
    cases["parse"].append({"raw": raw, "found": jv.journeys_json(found), "problems": problems})
for t in ["```journeys\n[not json\n```", "```journeys\n{\"a\": }\n```", "```journeys\n[1,]\n```", "```journeys\n\n```",
          "```journeys\n[\"\\x\"]\n```", "```journeys\n[1] 2\n```", "x```journeys \t\n[]```", "```Journeys\n[{\"name\":1}]\n```",
          "```journeys\n[]\n```\n```journeys\n[{\"name\": \"b\", \"steps\": [{\"do\": \"tap\", \"target\": \"x\"}], \"expect\": [\"y\"]}]\n```", "no block"]:
    found, problems = jv.extract_journeys(t)
    cases["extract"].append({"text": t, "found": None if found is None else jv.journeys_json(found), "problems": problems})
for _ in range(200):
    kind = rng.choice(["main", "back", "field", "select", "item", "radio", "tab", "checkbox", "link", "button", "x"])
    c = jv.Control("1", kind, s(6), type=rng.choice(["", "number", "date", "time", "textarea", "search", "email", "tel"]),
                   placeholder=rng.choice(["", "e.g. 30", "e.g.", " e.g.  Alex ", s(5)]), options=tuple(s(3) for _ in range(rng.randint(0, 10))),
                   on=rng.random() < 0.5)
    cases["render"].append({"kind": c.kind, "label": c.label, "type": c.type, "placeholder": c.placeholder, "options": list(c.options), "on": c.on, "got": c.render()})
json.dump(cases, open(sys.argv[1], "w"), ensure_ascii=False, separators=(',', ':'))
