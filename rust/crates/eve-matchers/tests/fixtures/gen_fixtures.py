"""Regenerate parity.json from the Python matchers module (Python 3.11+).

    python3 rust/crates/eve-matchers/tests/fixtures/gen_fixtures.py

Records what Python's `re` and `matchers` answer, so the Rust tests can
assert the same answers without a Python interpreter.
"""
import json
import random
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[4] / "bridge" / "src"))
from eve_bridge import matchers  # noqa: E402

TEXTS = [
    "", "ls", "ls\n", "ls\n\n", "rm", "rm\n", "rm -rf x", "rm\nx", "pwd", "pwd\n", "id\n",
    "git branch -vd old", "git branché -d x", "git branch -d\n", "mkfs.ext4 /dev/x",
    "mkfś", "find . -delete", "find . -deleted", "find . -deleteé", "find . -exec",
    "xargs -0 rm", "xargsrm", "xargs érm", "> /dev/null", ">\x1c/dev/null", ">　/dev/x",
    "pytest", "pytest -q", "pytest -q\n", "pytest\x1c-v", "pytest -v\x1d", "pytest -v",
    "a{", "xa{", "a{1,b", "aab", "x}", "]", "[", "&", "~", "-", "+", ",", "a--b", "\x08", "\x07",
    "A", "a", "K", "K", "²", "_", "́", "‿", "ab", "a b", "ab\n", "a\nb",
    "abc", "été", "x9", "٣", "1", "01", "a1", "\x01", "\n", "\t", "\x0b", "\x1c",
    "word", " word ", "wórd", "#c", "a#b", "%", "é", "<", ">", "a\\b", "z", "Z",
    "kK", "\u212a", "\u00c9", " ", "(", "xx", "ls -la", "cat\n", "aa", "ba", "a\n",
    "\u0130", "\u0131", "I", "i", "\u1e9e", "\u00df", "\u017f", "s", "\u03c2", "\u03a3", "x\u0130", "\u0345",
]

PATTERNS = list(matchers.DEFAULT_AUTO_ALLOW) + list(matchers.DEFAULT_ALWAYS_ASK) + [
    r"^ls$", r"^ls\Z", r"ls$", r"(?m)^b$", r"(?m)ls$", r"\Aa", r"a\Z", r"\z", r"\e", r"\h",
    r"\k<x>", r"\<", r"\>", r"a{", r"a{1,b", r"a{,2}b", r"x}", r"a{}", r"x{,}", r"a{2,1}", r"{2}",
    r"[]a]", r"[^]a]", r"[a[]", r"[&&a]", r"[a&&b]", r"[~~]", r"[+--]", r"[a-]", r"[\b]",
    r"[\w]", r"[\W]", r"[^\w]", r"[\s]", r"[\S]", r"[\d]", r"[\D]", r"\w", r"\W", r"\s", r"\S",
    r"\d", r"\D", r"\bword\b", r"\Bor\B", r"\b", r"\B", r"(?a)\w", r"(?a)\s", r"(?a)\bab\b",
    r"(?i)k", r"(?i)K", r"(?u)a", r"(?L)a", r"(?au)a", r"a(?i)b", r"(?i:A)b", r"(?-i:a)",
    r"(?x) a b # comment", r"(?x)a\ b", r"(?x)[ ]", r"(?#c)ab", r"a(?#c\)b", r"(?P<n>a)(?P=n)",
    r"(a)\1", r"\1", r"(a)\10", r"\0", r"\012", r"\x41", r"\x4", r"é", r"\U0001F600",
    r"\U00110000", r"\N{LATIN SMALL LETTER A}", r"\%", r"\é", r"[\1]", r"[\8]", r"[\x41-\x5a]",
    r"\8", r"\a", r"\f", r"\v", r"\t", r"\n", r"\r", r"a\\b", r"[unclosed", r"(", r")", r"a**",
    r"a++", r"a?+", r"(?>a)b", r"(?<=a)b", r"(?<!a)b", r"(?<=a|bc)d", r"(?<n>a)", r"(?P>n)",
    r"(?=a)", r"(?!a)", r"a|", r"|", r"", r".", r"(?s).", r"^", r"$", r"#c", r"a#b", r"\#",
    "\\", r"[a-\w]", r"(?(1)a|b)", r"(a)?(?(1)b|c)", r"\ud800", r"[[:alpha:]]", r"[a&b]",
    r"^.*", r"\bab", r"ab\b", r"\d+\s*$", r"(?m)$", r"(?m)^", r"[$]", r"\$", r"x$\n",
    r"(?<=\b)a", r"(?<=(a))b\1", r"(a)(?<=\1)b", r"a{2}?", r"a{2}+", r"a*?", r"(?i)[a-z]+",
    r"(a|bc){2}", r"(?<=(?:ab|cd))x", r"(?<=a{2})b", r"(?<=a*)b", r"(?<=a{1,2})b", r"x{3,}",
    r"^*", r"\b+", r"(?=a)*", r"a|*", r"()", r"(?:)", r"(?(1)a)", r"(a)(?(1)b|c|d)",
    r"(?P<n>a)(?(n)b)", r"(a)(?(1)b)", r"(a)(?(0)b)", r"[\]]", r"[a\]]", r"[\-]", r"[a-z-]",
    r"[-a]", r"[\\]", r"[^\]]", r"[--a]", r"(?x)[ # ]", r"(?x)a{2 }", "(?x)a #c\n b",
    r"(?x)\ ", r"(?s:.)", r"(?i-s:a)", r"(?-:a)", r"(?a:\w)", r"(?x:a b)", r"(?i)(?-i:a)",
    r"a)", r"(a", r"(?P<1>a)", r"(?P<n>a)(?P=n)", r"(?P=n)", r"a{4294967295}", r"a{99999}",
    r"\0777", r"\400", r"[\400]", r"[\777]", r"\11", r"(a)(b)(c)(d)(e)(f)(g)(h)(i)(j)\10",
    r"(a)\1a", r"(a)\10", r"(?i)\u00c9", r"(?i)é", r"[^\d\s]", r"[\D]+", r"\W+", r"\S+$",
    r"(?m)\Z", r"^git branch\b", r"\x7f", r"[\x00-\x1f]", r"(?u)\w", r"(?a)\d", r"(?a)\D",
    r"(?ia)k", r"(?<!\w)k", r"x*+", r"(?>a+)a", r"a+?b", r"a??", r"a{,3}?",
    r"(?ai)[a-c]", r"(?ai)[^a-c]", r"(?ai)\u00e9", r"(?ai)\x4b", r"(?i:(?a:k))", r"(?a)(?i:k)",
    r"(?i)(?a:k)", r"(?=a)+", r"(?=a)?", r"(?=a)*?", r"(?!a)*b", r"(?=(a))?\1", r"(?m)(?-m:a$)",
    r"(?m:a$)", r"(?s)(?-s:.)", r"(?x)(?-x: a)", "(?x: [ ]#c\n)", r"(?i)(?-i:A)", r"a(?x:\ )",
    r"[\w-]", r"[\w-a]", r"[a-\d]", r"[z-a]", r"(?P<a1>x)(?P=a1)", r"(?P<a>x)(?P<a>y)",
    r"(?P<a>x(?P=a))", r"(?(a)x)", r"(?P<\u00e9>x)", "(?P<\u00e9>x)", r"(?a:\b)k", r"(?a:\s)",
    r"[\ud800-\udfff]", r"[^\ud800]", r"[\x00-\U0010ffff]", r"(?i)[^k]", r"(?ai)[^k]",
    r"(?x)a{1, 2}", r"(?x)a {2}", r"\(", r"[(]", r"(?:a|b)+$", r"^(?:ls|cat)( |$)",
    # Global flags may follow a comment, and whitespace or a comment in verbose mode.
    r"(?#x)(?i)k", "(?x) (?i)k", "(?x)#c\n(?i)k", r"(?#x)(?a)\w", r"(?#x", r"a(?#x)(?i)b",
    # An empty or look-around-only group can be repeated.
    r"(?:)*", r"(?:)?a", r"(?:(?:))+", r"(?:(?=a))+", r"(?:(?=a))*b", r"(?:){2}", r"()*",
    # Python's IGNORECASE: lowercase plus the extra cases, not Unicode folding.
    r"(?i)i", r"(?i)I", r"(?i)\u0130", r"(?i)\u0131", r"(?i)[a-k]", r"(?i)[^a-k]", r"(?i)[^k[A-Z]",
    r"(?i)\u00df", r"(?i)\u1e9e", r"(?i)s", r"(?i)\u017f", r"(?i)[r-t]", r"(?i)\u03c3", r"(?i)\u0345",
    r"(?i)\w\u0130", r"(?i)[\u0100-\u017f]", r"(?ai)i", r"(?ai)[h-j]", r"(?i:(?a:i))",
    r"(?i)[\x00-\U0010ffff]", r"(?i)[^\x00-\u00ff]",
    # A look-around is atomic, and inside a look-behind a group reference must
    # name a closed group defined before it.
    r"(?=|())\1", r"(?=(a)|(a))\2", r"(?<=(?(1)a|b))(a)", r"(a)(?<=(?(1)a|b))x",
    r"(?<=(a)\1)", r"(?<=\1)(a)", r"(?P<n>a)(?<=(?P=n))", r"(?<=(?P<n>a)(?P=n))",
    r"(?<=(?(n)a|b))(?P<n>a)",
]


def try_compile(p):
    """The compiled pattern, "error" for re.error, "overflow" for the
    OverflowError matchers._compile does not catch."""
    try:
        return re.compile(p)
    except re.error:
        return "error"
    except OverflowError:
        return "overflow"


def regex_cases():
    out = []
    for p in PATTERNS:
        rx = try_compile(p)
        if isinstance(rx, str):
            out.append({"pattern": p, "compiles": rx, "matches": []})
            continue
        out.append({
            "pattern": p,
            "compiles": "ok",
            "matches": [bool(rx.search(t)) for t in TEXTS] if rx else [],
        })
    return out


def simple_corpus():
    pieces = ["ls", "echo", "env", "find", "fd", "rg", "tree", "git", "log", "x", "FOO=1",
              "-exec", "-execdir", "-ok", "-Hx", "-ao", "-la", "--pre=sh", "--output",
              "-c", "*.py", ";", "&&", "|", ">", "$(", "`", "'", '"', "\\", "#", "\t", "\n", "{", "!",
              "rm", "cat", "git branch", "-d", " ", "\x1c", "　", "status", "/tmp", "mv",
              "curl", "sudo", "pytest", "-q", "-delete", "xargs", "push", "-vd", "--force", ""]
    rng = random.Random(20260925)
    corpus = [" ".join(rng.choice(pieces) for _ in range(rng.randint(1, 4)))
              for _ in range(3000)]
    corpus += ["", " ", "env", "env x", "ls -la", "tree -L 2", "rm\n", "ls\n", "pwd\n", "ls ",
               "ls\u0085", "ls\x1f", "ls -la"]
    return corpus


cfg = matchers.load_config(path=Path("/nonexistent.toml"))
strict = matchers.MatcherConfig(cfg.auto_allow, cfg.always_ask, strict=True)
corpus = simple_corpus()
fixture = {
    "texts": TEXTS,
    "regex": regex_cases(),
    "commands": [
        {"command": c, "is_simple": matchers.is_simple(c),
         "default": matchers.classify_command(c, cfg),
         "strict": matchers.classify_command(c, strict)}
        for c in corpus
    ],
}
(HERE / "parity.json").write_text(json.dumps(fixture, ensure_ascii=True, indent=0) + "\n")
print("wrote", HERE / "parity.json", len(fixture["regex"]), "patterns", len(corpus), "commands")
