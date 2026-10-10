#!/usr/bin/env python3
"""Score eve's memory end to end: the real nightly dream builds the stores, the real Telegram brain answers.

    uv run python tools/memory_eval.py --estimate --split dev         # cost and time, no model call
    uv run python tools/memory_eval.py --split dev --out dev.json     # build (cached) and score the dev split
    uv run python tools/memory_eval.py --seal-check                   # the holdout's sha256 against its seal
    uv run python tools/memory_eval.py --split holdout --out h.json   # refused unless the seal holds
    uv run python tools/memory_eval.py --split dev --features hybrid,retry --out f.json   # with search switches

The design is the memory plan's section 5 (2026-10-08). The data is synthetic only: a persona, 60 days of its
transcripts and the questions, under ``tests/fixtures/memory_eval/``. Nothing here reads or writes the owner's
memory: every memory folder this tool opens is checked to sit inside the system temp folder (``assert_temp``).

### The data

- ``persona.json``: the synthetic owner (only hashed here; the transcripts carry it).
- ``transcripts/<YYYY-MM-DD>.jsonl``: one ``transcripts.py`` line per row
  (``{"ts", "ch", "conv", "who", "kind", "text"[, "tool"]}``). Copied as is into a temp memory folder.
- ``questions.json``: the header fields (``split_rule``, ``evidence``, ``answer_keys``, ...: the format, in
  words) at the top level (or under ``"header"``), and ``"questions": [{"id", "category", "asked_on", "question",
  "answer_keys", "abstain", "evidence", "split", "forget"?, "leak_keys"?}]``.

  - ``answer_keys``: every element must appear in the normalized answer. An element that is a list is a choice:
    any one of its strings is enough (``["thursday", ["7 pm", "19:00"]]``). ``{"any": [...]}`` means the same.
  - ``evidence``: the lines whose text proves the answer. Each is ``{"day", "line"}``: ``line`` is the 0-based
    index of the line in ``transcripts/<day>.jsonl``, every line counted, close markers too (``{"day",
    "index"}`` and ``"<day>:<n>"`` are the same 0-based index; ``{"text"}`` or a plain string is the text).
  - ``forget``: the line the owner asks eve to forget before this question, a reference as in ``evidence``.
    ``leak_keys``: the words of the forgotten fact; the answer must hold none of them.
  - ``asked_on``: ``YYYY-MM-DD`` (asked at ``ASK_HOUR`` in the data's own time zone) or an ISO datetime.

- ``questions.sha256``: the seal. ``--split holdout`` refuses to run unless questions.json hashes to it.

**The split rule**, as the data's header writes it: a question is dev when the first hex digit of
``sha256(id)`` is 0-4 (5 of 16, about 31%), else holdout. Each question also carries that rule's result in
``"split"``; a question whose field disagrees with the rule stops the run (``SplitMismatch``).

### Building the memory, as the daemon would

The transcripts are copied into a temp ``EVE_MEMORY_DIR``; then, as ``daemon._build_memory`` and
``_start_dream`` do: ``records.ensure_repo``, ``Transcripts``, ``mem0_memory`` (when the owner's env has it on
and mem0ai is installed), ``records.make_client`` (the dream's cheap model) and ``dream.Dreamer``. Every day is
dreamt once, oldest first (``Dreamer.dream``: reconcile, consolidate, mem0 ingest and tidy, the ledger), a
failed night tried again up to ``dream.MAX_TRIES`` times. After each night the records and the mem0 index are
kept as a snapshot (``snapshots/<day>/``), so a question sees memory as it stood when it was asked. The result is
cached under the temp folder by a key of
the data's sha256 (persona and transcripts, never the questions) and the dream's models, so dev and holdout reuse
one build. The cost estimate is printed before anything is spent, and a build over ``--max-build-usd`` refuses.

### Asking

Each question goes through ``TelegramInlet._think``, the live brain's own loop, with the model and effort from
the owner's env file (loaded as ``tools/chief_eval.py`` does), the memory tools from ``Memory.tools()`` and the
profile and today block the daemon would put in the prompt. The clocks (the transcripts' wall, the turn's
system context and the opening brief) read ``asked_on``. The tool loop runs at most ``MAX_ROUNDS`` rounds, then
the brain's own out-of-rounds summary. Only memory tools run, through ``TelegramInlet._memory_tool``; any other
tool, and ``forget_apply`` (which would change the shared copy), answers "unavailable" and is recorded. The one
eval-only change is a developer message after the question asking for ``{"answer", "abstain", "source"}``.

**Memory as of the question.** Every question gets its own fresh copy in the run's temp folder, never the
cache: the records and mem0 from the snapshot of the last night dreamt before ``asked_on`` (the dream runs at
04:30, so a question asked on day D sees nights up to D-1), and the transcripts of the days up to D, with day D
cut at the asking moment. Nothing said after the question is in its memory.

A ``forget`` question runs the real two-step forget (``Memory.forget_preview`` then ``forget_apply`` with its
token) on that copy before asking: once with the forgotten line's whole text, then once per leak key as a quoted
phrase (the owner's "forget that", reaching whatever the dream wrote from the line). The cache is never touched.

### Features

``--features`` names the memory switches this run turns on (``memory_index.SWITCHES``: hybrid, timeargs, retry, or
``all``); every switch it does not name is set off, whatever the owner's env file says, so a row says exactly what
was on. A switch rebuilds only what it changes. The three search switches leave the dream's build alone: hybrid
adds one vector seed per data (``<cache>/<data key>.hybrid-v<N>/``, every round of the 60 days embedded once, the
rounds themselves dropped), and each question's fresh copy gets that seed and indexes its own transcripts against
it, so nothing said after the question is in its index and nothing is embedded twice. The two record switches
(validity, sources: ``RECORD_SWITCHES``) change what the dream writes, so the ones that are on are part of the
build's cache key: a run with one on dreams its own build once, and a run with both off reuses the plain build.

### Scoring, by code only

Normalize (lowercase, dates to ``YYYY-MM-DD``, ``$`` and thousands commas dropped, punctuation to spaces except a
``.`` or ``:`` between digits) and require every answer key. A key must start at a word start; a key ending in a
digit must end where the number ends (``"4"`` is not in ``"14"`` or ``"4.5"``; ``"12.6"`` is in ``"12.60"``), and
a key ending in a letter may run on (``"cashew"`` is in ``"cashews"``, ``"drop"`` in ``"dropped"``). An abstain
question scores 1 only on ``abstain == true``, and a forget question also only when its answer holds no leak key;
any other question that abstains scores 0; a reply that is not the JSON object scores 0 (``format``). Retrieval is scored apart: did any
evidence line's text appear in any tool result. A wrong answer is classed as EvalMem splits it: ``answer`` (the
keys reached the brain, in its prompt or a tool result), ``retrieval`` (stored, never reached) or ``storage``
(in none of the stores).

The output rows carry ids, categories and scores, never a question's text. The full trace (every tool call and
result) goes to a file in the run's temp folder unless ``--trace`` names one.
"""

from __future__ import annotations

import argparse
import asyncio
import hashlib
import json
import math
import os
import re
import shutil
import sys
import tempfile
import time
from dataclasses import dataclass, field
from datetime import date, datetime, timedelta, timezone, tzinfo
from pathlib import Path
from typing import Any, Callable, Iterable, Mapping, Optional, Sequence

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

DATA_DIR = Path(__file__).resolve().parents[1] / "tests" / "fixtures" / "memory_eval"
QUESTIONS = "questions.json"
SEAL = "questions.sha256"
PERSONA = "persona.json"
TRANSCRIPTS = "transcripts"
BUILD_FILE = "BUILD.json"
CACHE_NAME = "eve-memory-eval-cache"
BUILD_VERSION = 2                    # bump when the build steps change: a new key, a new build
SNAPS = "snapshots"                  # <build>/snapshots/<day>/{records,mem0}: memory after that night's dream

MAX_ROUNDS = 4                       # tool rounds per question (the daemon allows telegram.MAX_TOOL_ROUNDS)
ASK_HOUR = 20                        # a date-only asked_on is asked at 20:00 in the data's time zone
ATTEMPTS = 3                         # a request that fails to arrive is sent again; an answer never is
EVIDENCE_PROBE_CHARS = 120           # a search hit is clipped at 300 chars: match on the line's start
DEFAULT_MAX_BUILD_USD = 10.0
DAEMON_ENV = ("PATH", "HOME", "TMPDIR", "EVE_SERIAL_PORT", "EVE_ENV_FILE")
UNAVAILABLE = {"ok": False, "reason": "that tool is unavailable right now"}

EVAL_NOTE = (
    "This message is part of a memory evaluation. Answer it as you normally would, using your memory tools "
    "when you need them, but write your final reply as exactly one JSON object and nothing else, with these "
    'three keys: "answer" (a string: the answer itself, short: names as they were said, numbers in digits, dates as year-month-day), '
    '"abstain" (a boolean: true when your memory does not hold the answer, and then "answer" is ""), and '
    '"source" (a string: where the answer came from, for example the profile, today\'s conversation, or a '
    "memory search and the day it found).")

# Estimate assumptions (tokens unless named). Printed with the estimate: they are guesses, not measurements.
EST = {
    "record_tokens_per_day": 350,    # records grow this much per dreamt day, up to the consolidate cap
    "profile_tokens": 1500,
    "reconcile_out": 2500,
    "consolidate_out": 1200,
    "mem0_extract_prompt": 1800,
    "mem0_extract_out": 300,
    "mem0_update_in": 1200,
    "mem0_update_out": 300,
    "mem0_embed_extra": 200,
    "ask_calls_per_question": 2.5,
    "ask_out_per_call": 300,
    "tool_schema_chars": 6000,
    "reconcile_secs": 25.0,
    "consolidate_secs": 15.0,
    "mem0_chunk_secs": 6.0,
    "ask_call_secs": 5.0,
}


# ---- the temp-folder guard ----------------------------------------------------------------------------------

class UnsafeDir(RuntimeError):
    """A memory folder outside the system temp folder: the owner's memory must never be opened."""


def temp_roots() -> list[Path]:
    roots = {Path(os.path.realpath(tempfile.gettempdir()))}
    for p in ("/tmp", "/private/tmp"):
        if os.path.isdir(p):
            roots.add(Path(os.path.realpath(p)))
    return sorted(roots)


def assert_temp(path: Path) -> Path:
    """`path`, resolved, when it is strictly inside a temp root; UnsafeDir otherwise."""
    real = Path(os.path.realpath(Path(path).expanduser()))
    if not any(root in real.parents for root in temp_roots()):
        raise UnsafeDir(f"refusing a memory folder outside the temp folder: {real}")
    owner = Path(os.path.realpath(Path("~/.config/eve-bridge").expanduser()))
    if real == owner or owner in real.parents:
        raise UnsafeDir(f"refusing the owner's own eve folder: {real}")
    return real


# ---- the data ------------------------------------------------------------------------------------------------

@dataclass
class Dataset:
    root: Path
    persona: dict[str, Any]
    days: list[str]
    header: dict[str, Any] = field(default_factory=dict)
    questions: list[dict[str, Any]] = field(default_factory=list)


def _files(folder: Path) -> list[Path]:
    return sorted(p for p in folder.rglob("*") if p.is_file() and not p.is_symlink())


def load_dataset(root: Path, *, questions: bool = True) -> Dataset:
    root = Path(root)
    persona = json.loads((root / PERSONA).read_text(encoding="utf-8"))
    days = sorted(p.stem for p in (root / TRANSCRIPTS).glob("*.jsonl") if re.match(r"^\d{4}-\d{2}-\d{2}$", p.stem))
    if not days:
        raise SystemExit(f"NO_DATA: no transcripts/<day>.jsonl under {root}")
    ds = Dataset(root, persona if isinstance(persona, dict) else {}, days)
    if questions:
        raw = json.loads((root / QUESTIONS).read_text(encoding="utf-8"))
        ds.header = dict(raw.get("header") or {k: v for k, v in raw.items() if k != "questions"})
        ds.questions = [q for q in raw.get("questions") or [] if isinstance(q, dict)]
        for q in ds.questions:
            split_of(q)                      # a precomputed split that breaks the rule stops here
    return ds


def data_sha(root: Path) -> str:
    """sha256 over the persona and every transcript file (names and bytes). Never the questions."""
    h = hashlib.sha256()
    for p in [Path(root) / PERSONA, *_files(Path(root) / TRANSCRIPTS)]:
        h.update(str(p.relative_to(root)).encode() + b"\0" + p.read_bytes() + b"\0")
    return h.hexdigest()


def build_key(root: Path) -> str:
    """The build's cache key: the data, the build steps, the dream's models, and the record switches that are on
    (they change what the dream writes). With every record switch off the key is what it was before they existed."""
    from eve_bridge import mem0_memory, records

    tag = f"v{BUILD_VERSION}|{records.DEFAULT_MODEL}|{mem0_memory.DEFAULT_MODEL}|{mem0_memory.DEFAULT_EMBED_MODEL}"
    on = [name for name, var in RECORD_SWITCHES.items() if records._switch(var)]
    if on:                     # the switches, and the dream's words for them: a prompt change is a new build
        v, s = "validity" in on, "sources" in on
        words = hashlib.sha256((records.reconcile_prompt(v, s) + records.consolidate_prompt(v, s)).encode())
        tag += "|records:" + ",".join(on) + "|" + words.hexdigest()[:12]
    return hashlib.sha256((data_sha(root) + "|" + tag).encode()).hexdigest()[:24]


def sha256_file(path: Path) -> str:
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def seal_check(root: Path) -> tuple[bool, str, str]:
    """(ok, sha256 of questions.json, the sealed one)."""
    digest = sha256_file(Path(root) / QUESTIONS)
    try:
        sealed = (Path(root) / SEAL).read_text(encoding="utf-8").split()[0].strip().lower()
    except (OSError, IndexError):
        sealed = ""
    return bool(sealed) and digest == sealed, digest, sealed


class SplitMismatch(ValueError):
    """A question's own "split" field disagrees with the split rule."""


def rule_split(qid: Any) -> str:
    """The data's rule: dev when the first hex digit of sha256(str(id)) is 0-4, else holdout."""
    return "dev" if hashlib.sha256(str(qid).encode("utf-8")).hexdigest()[0] in "01234" else "holdout"


def split_of(q: Mapping[str, Any]) -> str:
    want = rule_split(q.get("id"))
    own = q.get("split")
    if own is not None and own != want:
        raise SplitMismatch(f"question {q.get('id')}: split {own!r}, the rule says {want!r}")
    return want


def select(ds: Dataset, split: str, limit: Optional[int] = None) -> list[dict[str, Any]]:
    out = [q for q in ds.questions if split_of(q) == split]
    return out[:limit] if limit else out


# ---- normalization and scoring --------------------------------------------------------------------------------

_MONTHS = {"january": 1, "february": 2, "march": 3, "april": 4, "may": 5, "june": 6, "july": 7, "august": 8,
           "september": 9, "october": 10, "november": 11, "december": 12, "jan": 1, "feb": 2, "mar": 3, "apr": 4,
           "jun": 6, "jul": 7, "aug": 8, "sep": 9, "sept": 9, "oct": 10, "nov": 11, "dec": 12}
_MON = "|".join(sorted(_MONTHS, key=len, reverse=True))
_ISO = re.compile(r"\b(\d{4})-(\d{1,2})-(\d{1,2})\b")
_MDY = re.compile(r"\b(\d{1,2})/(\d{1,2})/(\d{4})\b")
_MON_D = re.compile(rf"\b({_MON})\.?\s+(\d{{1,2}})(?:st|nd|rd|th)?\b(?:,?\s+(\d{{4}})\b)?", re.I)
_D_MON = re.compile(rf"\b(\d{{1,2}})(?:st|nd|rd|th)?\s+(?:of\s+)?({_MON})\b\.?(?:,?\s+(\d{{4}})\b)?", re.I)


def _iso(y: Optional[int], m: int, d: int, raw: str) -> str:
    if y is None:
        return raw
    try:
        return date(y, m, d).isoformat()
    except ValueError:
        return raw


def dates_to_iso(text: str, year: Optional[int] = None) -> str:
    """Every date written as a date to YYYY-MM-DD. A date with no year takes `year`; with no `year` it stays."""
    text = _ISO.sub(lambda m: _iso(int(m[1]), int(m[2]), int(m[3]), m[0]), text)
    text = _MDY.sub(lambda m: _iso(int(m[3]), int(m[1]), int(m[2]), m[0]), text)
    text = _MON_D.sub(lambda m: _iso(int(m[3]) if m[3] else year, _MONTHS[m[1].lower()], int(m[2]), m[0]), text)
    return _D_MON.sub(lambda m: _iso(int(m[3]) if m[3] else year, _MONTHS[m[2].lower()], int(m[1]), m[0]), text)


def normalize(text: Any, year: Optional[int] = None) -> str:
    s = dates_to_iso(str(text or ""), year).lower()
    s = re.sub(r"(?<=\d),(?=\d{3}\b)", "", s)
    s = re.sub(r"[^\w\s.:-]", " ", s.replace("_", " "))
    s = re.sub(r"(?<!\d)[.:]|[.:](?!\d)", " ", s)      # a . or : stays only between digits: 5.75, 11:30
    s = re.sub(r"(?<!\d)-|-(?!\d)", " ", s)
    return " ".join(s.split())


def _key_re(key: str) -> "re.Pattern[str]":
    """Where a normalized key may sit: from a word start; a number ends where the number ends (trailing zeros
    after a decimal point aside); a word may run on (plurals, -ed, -ing)."""
    if key[0].isdigit():
        left = r"(?<![\d.])(?<!\d[-:])"
    else:
        left = r"(?<!\w)"
    right = ""
    if key[-1].isdigit():
        right = ("0*" if re.search(r"\.\d+$", key) else r"(?:\.0+)?") + r"(?!\d|[.:-]\d)"
    return re.compile(left + re.escape(key) + right)


def _has(needle: str, hay: str) -> bool:
    return bool(needle) and _key_re(needle).search(hay) is not None


def keys_present(keys: Any, text: str, year: Optional[int] = None) -> bool:
    """Every answer key in `text` (normalized). A key that is a list, or {"any": [...]}, needs one member."""
    hay = normalize(text, year)
    items = keys if isinstance(keys, list) else [keys]
    if not items:
        return False

    def one(k: Any) -> bool:
        if isinstance(k, list):
            return any(one(x) for x in k)
        if isinstance(k, Mapping) and isinstance(k.get("any"), list):
            return any(one(x) for x in k["any"])
        return _has(normalize(k, year), hay)

    return all(one(k) for k in items)


def leaked(q: Mapping[str, Any], text: str) -> bool:
    """Any of a forget question's leak keys in `text`."""
    keys = q.get("leak_keys") or []
    return bool(keys) and keys_present([list(keys)], text, asked_year(q))


def parse_reply(text: str) -> Optional[dict[str, Any]]:
    """The reply's JSON object {"answer", "abstain", "source"}, or None when there is none."""
    s = (text or "").strip()
    s = re.sub(r"^```(?:json)?\s*|\s*```$", "", s)
    for cand in [s] + [m.group(0) for m in re.finditer(r"\{.*\}", s, re.S)]:
        try:
            obj = json.loads(cand)
        except ValueError:
            continue
        if isinstance(obj, dict) and "answer" in obj and isinstance(obj.get("abstain"), bool):
            return {"answer": str(obj.get("answer") or ""), "abstain": obj["abstain"],
                    "source": str(obj.get("source") or "")}
    return None


def asked_year(q: Mapping[str, Any]) -> Optional[int]:
    m = re.match(r"^(\d{4})-", str(q.get("asked_on") or ""))
    return int(m.group(1)) if m else None


def score(q: Mapping[str, Any], reply: Optional[Mapping[str, Any]]) -> int:
    """1 or 0. No JSON reply: 0. An abstain question: the abstain flag (and no leak key in the answer). Any
    other: no abstain and every key."""
    if reply is None:
        return 0
    if bool(q.get("abstain")):
        return int(reply.get("abstain") is True and not leaked(q, str(reply.get("answer") or "")))
    if reply.get("abstain") is True:
        return 0
    return int(keys_present(q.get("answer_keys") or [], str(reply.get("answer") or ""), asked_year(q)))


# ---- evidence -----------------------------------------------------------------------------------------------

_REF = re.compile(r"^(\d{4}-\d{2}-\d{2})[:#L]+(\d+)$")


def _line_text(tdir: Path, day: str, index: int) -> Optional[str]:
    try:
        rows = (tdir / f"{day}.jsonl").read_text(encoding="utf-8").splitlines()
        obj = json.loads(rows[index]) if 0 <= index < len(rows) else None
    except (OSError, ValueError, IndexError):
        return None
    return str(obj.get("text")) if isinstance(obj, dict) and obj.get("text") else None


def ref_text(tdir: Path, ref: Any) -> Optional[str]:
    """The text an evidence or forget reference points at (formats in the module text), or None."""
    if isinstance(ref, str):
        m = _REF.match(ref.strip())
        return _line_text(tdir, m.group(1), int(m.group(2))) if m else (ref.strip() or None)
    if isinstance(ref, Mapping):
        if isinstance(ref.get("text"), str) and ref["text"].strip():
            return ref["text"].strip()
        day = str(ref.get("day") or "")
        for k in ("line", "index"):                       # both 0-based, close markers counted
            if isinstance(ref.get(k), int) and not isinstance(ref.get(k), bool):
                return _line_text(tdir, day, ref[k])
    return None


def evidence_texts(q: Mapping[str, Any], tdir: Path) -> tuple[list[str], int]:
    """(resolved evidence texts, how many references did not resolve)."""
    refs = q.get("evidence") or []
    refs = refs if isinstance(refs, list) else [refs]
    got = [ref_text(tdir, r) for r in refs]
    return [t for t in got if t], sum(1 for t in got if not t)


def forget_queries(q: Mapping[str, Any], tdir: Path) -> Optional[list[str]]:
    """The forget queries for a forget question, or None: the forgotten line's whole text (a "query" or a plain
    string is taken as the words), then each leak key as a quoted phrase."""
    spec = q.get("forget")
    if spec is None or spec == "":
        return None
    if isinstance(spec, Mapping) and isinstance(spec.get("query"), str):
        first: Optional[str] = spec["query"].strip() or None
    elif isinstance(spec, str) and not _REF.match(spec.strip()):
        first = spec.strip() or None
    else:
        first = ref_text(tdir, spec)
    out = [first] if first else []
    out += [f'"{str(k).strip()}"' for k in q.get("leak_keys") or [] if str(k).strip()]
    return out


def strings(obj: Any) -> Iterable[str]:
    if isinstance(obj, str):
        yield obj
    elif isinstance(obj, Mapping):
        for v in obj.values():
            yield from strings(v)
    elif isinstance(obj, (list, tuple)):
        for v in obj:
            yield from strings(v)


def evidence_hit(evidence: Sequence[str], results: Sequence[Any]) -> bool:
    """Any evidence line's text (its normalized start) inside any tool result."""
    hay = " | ".join(normalize(s) for r in results for s in strings(r))
    for text in evidence:
        probe = normalize(text)[:EVIDENCE_PROBE_CHARS].rsplit(" ", 1)[0] if len(normalize(text)) > \
            EVIDENCE_PROBE_CHARS else normalize(text)
        if probe and probe in hay:
            return True
    return False


# ---- the memory: open, copy, forget -------------------------------------------------------------------------

class Clock:
    """The one wall clock every reader of the eval's memory shares: set to each question's asked_on."""

    def __init__(self, now: Optional[datetime] = None) -> None:
        self.now = now or datetime.now().astimezone()

    def __call__(self) -> datetime:
        return self.now


def data_tz(tdir: Path, days: Sequence[str]) -> tzinfo:
    """The time zone of the data's own timestamps (the first line's offset); local when none parses."""
    for day in days:
        try:
            for raw in (tdir / f"{day}.jsonl").read_text(encoding="utf-8").splitlines():
                ts = json.loads(raw).get("ts")
                at = datetime.fromisoformat(str(ts))
                if at.tzinfo is not None:
                    return at.tzinfo
        except (OSError, ValueError, AttributeError):
            continue
    return datetime.now().astimezone().tzinfo or timezone.utc


def asked_at(q: Mapping[str, Any], tz: tzinfo) -> datetime:
    raw = str(q.get("asked_on") or "").strip()
    if re.match(r"^\d{4}-\d{2}-\d{2}$", raw):
        d = date.fromisoformat(raw)
        return datetime(d.year, d.month, d.day, ASK_HOUR, 0, tzinfo=tz)
    at = datetime.fromisoformat(raw)
    return at if at.tzinfo is not None else at.replace(tzinfo=tz)


def make_index(cfg: Any) -> Any:
    """mem0 over this memory folder, as mem0_memory.shared would (the env's switch, mem0ai installed), or None."""
    from eve_bridge import mem0_memory

    mcfg = mem0_memory.configured(cfg)
    if not mcfg.enabled or not mem0_memory.available():
        return None
    return mem0_memory.OwnerMemory(mcfg)


def open_memory(store: Path, clock: Callable[[], datetime],
                index_factory: Optional[Callable[[Any], Any]] = make_index) -> Any:
    """memory.Memory over `store` (inside the temp folder), its transcripts on `clock`."""
    from eve_bridge import memory, transcripts
    from eve_bridge.recall import RecallConfig

    store = assert_temp(store)
    cfg = RecallConfig(store=store, notes=store / "notes")
    tr = transcripts.Transcripts(transcripts.TranscriptConfig(enabled=True, root=cfg.transcripts_dir),
                                 archive=cfg.archive_dir, wall=clock)
    if not tr.enabled:
        raise RuntimeError("the eval's transcripts did not open (see the warning above)")
    return memory.Memory(cfg, tr, index_factory(cfg) if index_factory else None)


def copy_memory(src: Path, dst: Path) -> Path:
    assert_temp(src)
    dst = assert_temp(dst)
    shutil.copytree(src, dst, symlinks=True)
    return dst


def run_forget(mem: Any, query: str) -> dict[str, Any]:
    """The real two-step forget: preview (counts and a token), then apply with that token. Counts only."""
    prev = mem.forget_preview(query)
    if not prev.get("ok") or not prev.get("token"):
        return {"ok": False, "previewed": int(prev.get("total") or 0), "forgot": 0}
    done = mem.forget_apply(prev["token"])
    return {"ok": bool(done.get("ok")), "previewed": int(prev.get("total") or 0),
            "forgot": int(done.get("total") or 0), "layers": done.get("layers") or {}}


def run_forgets(mem: Any, queries: Sequence[str], line_resolved: bool = True) -> dict[str, Any]:
    """Each query through the real forget, in order. ok: the forgotten line resolved and its own forget (the
    first query) matched and applied; a leak-key query that finds nothing left is not a failure."""
    out: dict[str, Any] = {"ok": bool(line_resolved and queries), "previewed": 0, "forgot": 0,
                           "queries": len(queries)}
    for i, query in enumerate(queries):
        r = run_forget(mem, query)
        out["previewed"] += r["previewed"]
        out["forgot"] += r["forgot"]
        if i == 0 and not (r["ok"] and r["forgot"]):
            out["ok"] = False
    return out


def memory_as_of(built: Path, tdir: Path, days: Sequence[str], at: datetime, dst: Path) -> tuple[Path, str]:
    """A fresh memory folder at `dst` holding what eve would hold at `at`: the records and mem0 of the last
    snapshot before the asking day, and the transcripts up to `at`. → (dst, the snapshot day or "")."""
    from eve_bridge import records, transcripts

    built = assert_temp(built)
    dst = assert_temp(dst)
    today = (at - timedelta(hours=transcripts.DAY_START_HOUR)).strftime("%Y-%m-%d")
    snaps = built / SNAPS
    prior = sorted(p.name for p in snaps.iterdir() if p.is_dir() and p.name < today) if snaps.is_dir() else []
    dst.mkdir(parents=True)
    if prior:
        for sub in ("records", "mem0"):
            if (snaps / prior[-1] / sub).exists():
                shutil.copytree(snaps / prior[-1] / sub, dst / sub, symlinks=True)
    if not (dst / "records").exists():
        records.ensure_repo(dst / "records")
    (dst / "transcripts").mkdir()
    for day in days:
        if day > today:
            continue
        src = tdir / f"{day}.jsonl"
        if day < today:
            shutil.copy2(src, dst / "transcripts" / src.name)
            continue
        keep = []
        for raw in src.read_text(encoding="utf-8").splitlines():
            try:
                ts = datetime.fromisoformat(str(json.loads(raw).get("ts")))
            except (ValueError, AttributeError):
                continue
            if ts.tzinfo is not None and ts <= at:
                keep.append(raw)
        (dst / "transcripts" / src.name).write_text("".join(r + "\n" for r in keep), encoding="utf-8")
    return dst, (prior[-1] if prior else "")


def stored_text(mem: Any) -> str:
    """Everything the stores hold, normalized: transcripts, records (and journal, stars, profile), mem0."""
    parts: list[str] = []
    for day in mem.transcripts.days():
        parts += [ln.get("text", "") for ln in mem.transcripts.lines(day)]
    rec = Path(mem.cfg.records_dir)
    if rec.is_dir():
        for p in sorted(rec.rglob("*.md")):
            if ".git" not in p.parts:
                parts.append(p.read_text(encoding="utf-8", errors="replace"))
    if mem.index is not None:
        try:
            parts += [r.get("text", "") for r in mem.index.find(lambda t: True)]
        except Exception:  # noqa: BLE001 - an index that cannot list counts as holding nothing
            pass
    return "\n".join(parts)


# ---- the estimate ---------------------------------------------------------------------------------------------

def _day_stats(tdir: Path, day: str) -> tuple[int, list[int], int]:
    """(chars the dream reads, mem0 chunks' chars, owner+eve say lines) for one day file."""
    from eve_bridge import mem0_memory, transcripts

    chars, convs = 0, {}
    lines = 0
    try:
        raw = (tdir / f"{day}.jsonl").read_text(encoding="utf-8").splitlines()
    except OSError:
        raw = []
    for r in raw:
        ln = transcripts._parse_line(r)
        if ln is None:
            continue
        if ln["kind"] in transcripts.RENDER_KINDS and ln["text"]:
            chars += len(ln["text"]) + 40
        if ln["kind"] == "say" and ln["who"] in ("owner", "eve") and ln["text"].strip():
            convs.setdefault(ln["conv"], []).append((ln["who"], len(ln["text"][:mem0_memory.MESSAGE_CHARS])))
            lines += 1
    chunks = []
    for msgs in convs.values():
        for i in range(0, len(msgs), mem0_memory.CHUNK_MESSAGES):
            part = msgs[i:i + mem0_memory.CHUNK_MESSAGES]
            if any(w == "owner" for w, _ in part):
                chunks.append(sum(n for _, n in part))
    return min(chars, transcripts.DAY_TEXT_CHARS), chunks, lines


def _usd(model: str, tin: float, tout: float) -> Optional[float]:
    from eve_bridge import pricing

    r = pricing.openai_rates(model)
    return None if r is None else (tin * r["input"] + tout * r["output"]) / 1e6


def estimate(ds: Dataset, questions: Sequence[Mapping[str, Any]], *, ask_model: str, about_chars: int,
             index_on: bool, cached: bool) -> dict[str, Any]:
    """Build and ask cost and time, from the data's sizes and the EST assumptions. No model call."""
    from eve_bridge import mem0_memory, pricing, records, telegram, transcripts

    tdir = ds.root / TRANSCRIPTS
    cap = records.MAX_CONSOLIDATE_CHARS / 4
    b = {"calls": 0, "in": 0.0, "out": 0.0, "embed": 0.0, "secs": 0.0, "mem0_calls": 0, "mem0_in": 0.0,
         "mem0_out": 0.0}
    dreamt = 0
    day_chars: dict[str, int] = {}
    for day in ds.days:
        chars, chunks, _ = _day_stats(tdir, day)
        day_chars[day] = chars
        if not chars:
            continue
        dreamt += 1
        rec = min(cap, EST["record_tokens_per_day"] * dreamt)
        b["in"] += len(records.RECONCILE_PROMPT) / 4 + rec + EST["profile_tokens"] + chars / 4
        b["in"] += len(records.CONSOLIDATE_PROMPT) / 4 + rec + EST["profile_tokens"]
        b["out"] += EST["reconcile_out"] + EST["consolidate_out"]
        b["calls"] += 2
        b["secs"] += EST["reconcile_secs"] + EST["consolidate_secs"]
        if index_on:
            for c in chunks:
                b["mem0_calls"] += 2
                b["mem0_in"] += EST["mem0_extract_prompt"] + c / 4 + EST["mem0_update_in"]
                b["mem0_out"] += EST["mem0_extract_out"] + EST["mem0_update_out"]
                b["embed"] += c / 4 * 0.3 + EST["mem0_embed_extra"]
                b["secs"] += EST["mem0_chunk_secs"]
    dream_usd = _usd(records.DEFAULT_MODEL, b["in"], b["out"]) or 0.0
    mem0_usd = (_usd(mem0_memory.DEFAULT_MODEL, b["mem0_in"], b["mem0_out"]) or 0.0) + \
        (pricing.estimate_embedding_cost(mem0_memory.DEFAULT_EMBED_MODEL, int(b["embed"])) or 0.0)
    build_usd = 0.0 if cached else dream_usd + mem0_usd

    instr = len(telegram.INSTRUCTIONS) + about_chars + EST["profile_tokens"] * 4 + EST["tool_schema_chars"]
    a_in = a_out = 0.0
    calls = 0.0
    tz = data_tz(tdir, ds.days)
    for q in questions:
        try:
            day = (asked_at(q, tz) - timedelta(hours=transcripts.DAY_START_HOUR)).strftime("%Y-%m-%d")
        except ValueError:
            day = ""
        today = min(telegram.TODAY_CHARS, day_chars.get(day, 0))
        n = EST["ask_calls_per_question"]
        calls += n
        a_in += n * ((instr + today) / 4 + 600)
        a_out += n * EST["ask_out_per_call"]
    ask_usd = _usd(ask_model, a_in, a_out)
    return {"days": len(ds.days), "days_with_words": dreamt, "build_cached": cached, "index_on": index_on,
            "dream_model": records.DEFAULT_MODEL, "dream_calls": b["calls"], "dream_in": int(b["in"]),
            "dream_out": int(b["out"]), "dream_usd": round(dream_usd, 4), "mem0_calls": b["mem0_calls"],
            "mem0_usd": round(mem0_usd, 4), "build_usd": round(build_usd, 4),
            "build_secs": 0.0 if cached else round(b["secs"]), "questions": len(questions),
            "ask_model": ask_model, "ask_calls": round(calls, 1), "ask_in": int(a_in), "ask_out": int(a_out),
            "ask_usd": None if ask_usd is None else round(ask_usd, 4),
            "ask_secs": round(calls * EST["ask_call_secs"])}


def print_estimate(e: Mapping[str, Any]) -> None:
    print("== estimate (no model called; assumptions in memory_eval.EST, tokens are chars/4)")
    print(f"   data: {e['days']} days, {e['days_with_words']} with words; mem0 index {'on' if e['index_on'] else 'off'}")
    if e["build_cached"]:
        print("   build: cached for this data, $0")
    else:
        print(f"   build: dream {e['dream_model']} {e['dream_calls']} calls ~{e['dream_in']:,} in / "
              f"{e['dream_out']:,} out = ${e['dream_usd']:.3f}; mem0 ~{e['mem0_calls']} calls + embeddings = "
              f"${e['mem0_usd']:.3f}; total ~${e['build_usd']:.3f}, ~{e['build_secs'] / 60:.0f} min")
    usd = "unpriced" if e["ask_usd"] is None else f"${e['ask_usd']:.3f} (no cache credit: an upper bound)"
    print(f"   ask: {e['questions']} questions on {e['ask_model']}, ~{e['ask_calls']} calls ~{e['ask_in']:,} in / "
          f"{e['ask_out']:,} out = {usd}, ~{e['ask_secs'] / 60:.0f} min")


# ---- the build ------------------------------------------------------------------------------------------------

def _spend_usd(folder: Path) -> float:
    from eve_bridge import spend

    rows: list[dict[str, Any]] = []
    for p in sorted(Path(folder).glob("*.jsonl")) if Path(folder).is_dir() else []:
        for raw in p.read_text(encoding="utf-8").splitlines():
            try:
                rows.append(json.loads(raw))
            except ValueError:
                continue
    return float(spend.totals(rows)["usd"])


def _close_index(index: Any) -> None:
    try:
        index._mem.vector_store.client.close()
    except Exception:  # noqa: BLE001 - best effort: the folder is renamed, never reused by this object
        pass


def _snapshot(work: Path, day: str) -> None:
    """The records and mem0 as they stand after `day`'s night, under <work>/snapshots/<day>/."""
    dst = work / SNAPS / day
    for sub in ("records", "mem0"):
        if not (work / sub).exists():
            continue
        # git's detached `gc --auto` after a commit can repack objects mid-copy (a vanished objects/<xx>): try again.
        for attempt in range(5):
            try:
                shutil.copytree(work / sub, dst / sub, symlinks=True)
                break
            except shutil.Error:
                shutil.rmtree(dst / sub, ignore_errors=True)
                if attempt == 4:
                    raise
                time.sleep(2.0)
    dst.mkdir(parents=True, exist_ok=True)


async def _dream_all(dreamer: Any, days: Sequence[str], tries: int,
                     after: Optional[Callable[[str], None]] = None) -> tuple[list[str], list[str]]:
    dreamt, failed = [], []
    for day in days:
        for _ in range(tries):
            report = await dreamer.dream(day)
            if report.dreamt:
                dreamt.append(day)
                if report.failed:
                    print(f"   {day}: dreamt; failed steps {', '.join(report.failed)}")
                break
        else:
            failed.append(day)
            print(f"   {day}: not dreamt after {tries} tries")
        if after is not None:
            after(day)
    return dreamt, failed


def build_memory(ds: Dataset, cache_root: Path, *, client: Any = None,
                 index_factory: Optional[Callable[[Any], Any]] = make_index) -> tuple[Path, dict[str, Any]]:
    """The built memory for this data (cached by build_key), and its manifest. Builds when missing."""
    from eve_bridge import dream, records, transcripts
    from eve_bridge.recall import RecallConfig

    cache_root = Path(cache_root)
    cache_root.mkdir(parents=True, exist_ok=True)
    assert_temp(cache_root / "x")
    key = build_key(ds.root)
    final = cache_root / key
    if (final / BUILD_FILE).is_file():
        return final, json.loads((final / BUILD_FILE).read_text(encoding="utf-8"))
    if client is None:
        client = records.make_client()
        if client is None:
            raise SystemExit("NO_DREAM_CLIENT: OPENROUTER_API_KEY is not set in the env file")
    work = assert_temp(Path(tempfile.mkdtemp(prefix=f"{key}.building-", dir=cache_root)))
    spend_dir = cache_root / f"{key}.spend"
    os.environ["EVE_SPEND_DIR"] = str(spend_dir)
    shutil.copytree(ds.root / TRANSCRIPTS, work / "transcripts", dirs_exist_ok=True)
    cfg = RecallConfig(store=work, notes=work / "notes")
    git = records.ensure_repo(cfg.records_dir)
    tr = transcripts.Transcripts(transcripts.TranscriptConfig(enabled=True, root=cfg.transcripts_dir),
                                 archive=cfg.archive_dir)
    if not tr.enabled:
        raise SystemExit("BUILD_FAILED: the transcripts did not open in the build folder")
    index = index_factory(cfg) if index_factory else None
    dreamer = dream.Dreamer(cfg, tr, client, index)
    t0 = time.monotonic()
    print(f"   building {key}: {len(tr.days())} days, records git {'on' if git else 'off'}, mem0 "
          f"{'on' if index is not None else 'off'}")
    dreamt, failed = asyncio.run(_dream_all(dreamer, tr.days(), dream.MAX_TRIES,
                                            after=lambda day: _snapshot(work, day)))
    manifest = {"key": key, "data_sha256": data_sha(ds.root), "days": len(tr.days()), "dreamt": len(dreamt),
                "failed_days": failed, "index": index is not None, "dream_model": getattr(client, "model", "?"),
                "secs": round(time.monotonic() - t0, 1), "usd": round(_spend_usd(spend_dir), 4),
                "built": datetime.now().astimezone().isoformat(timespec="seconds")}
    (work / BUILD_FILE).write_text(json.dumps(manifest, indent=1) + "\n", encoding="utf-8")
    if index is not None:
        _close_index(index)
    try:
        os.rename(work, final)
    except OSError:
        shutil.rmtree(work, ignore_errors=True)            # another run finished first: use its build
    return final, json.loads((final / BUILD_FILE).read_text(encoding="utf-8"))


# ---- features ---------------------------------------------------------------------------------------------------

# The record switches (records.py): they change what the dream writes, so they are in build_key and a run with one
# on dreams its own build. The search switches (memory_index.SWITCHES) leave the build alone.
RECORD_SWITCHES = {"validity": "EVE_MEMORY_VALIDITY", "sources": "EVE_MEMORY_SOURCES"}


def all_switches() -> dict[str, str]:
    """Every feature --features can name → its env variable: the search switches, then the record switches."""
    from eve_bridge import memory_index

    return {**memory_index.SWITCHES, **RECORD_SWITCHES}


def parse_features(raw: Optional[str]) -> list[str]:
    """The switches named by --features ("" for none; "all" for every one), in all_switches() order."""
    switches = all_switches()
    names = [n.strip().lower() for n in str(raw or "").split(",") if n.strip()]
    if "all" in names:
        return list(switches)
    unknown = [n for n in names if n not in switches]
    if unknown:
        raise SystemExit(f"UNKNOWN_FEATURE: the features are {', '.join(switches)} (or all)")
    return [n for n in switches if n in names]


def set_features(features: Sequence[str]) -> None:
    """Each switch on when named, off otherwise (the owner's env file never decides an eval row)."""
    for name, var in all_switches().items():
        os.environ[var] = "1" if name in features else "0"


def hybrid_seed(ds: Dataset, cache_root: Path) -> tuple[Path, dict[str, Any]]:
    """The vectors of every round in the data, embedded once and cached: (the seed file, its manifest). The seed
    holds vectors only (hash → vector); each question's copy builds its own rounds from its own transcripts."""
    from eve_bridge import memory_index, transcripts

    tag = f"{data_sha(ds.root)}|v{memory_index.INDEX_VERSION}|{memory_index.EMBED_MODEL}|{memory_index.EMBED_DIMS}"
    key = hashlib.sha256(tag.encode()).hexdigest()[:24]
    final = Path(cache_root) / f"{key}.hybrid-v{memory_index.INDEX_VERSION}"
    if (final / BUILD_FILE).is_file():
        return final / memory_index.DB_FILE, json.loads((final / BUILD_FILE).read_text(encoding="utf-8"))
    embed = memory_index.openai_embedder()
    if embed is None:
        raise SystemExit("NO_EMBEDDINGS: OPENROUTER_API_KEY is not set in the env file")
    work = assert_temp(Path(tempfile.mkdtemp(prefix=f"{key}.seeding-", dir=cache_root)))
    spend_dir = work / "spend"
    os.environ["EVE_SPEND_DIR"] = str(spend_dir)
    shutil.copytree(ds.root / TRANSCRIPTS, work / "store" / "transcripts")
    tr = transcripts.Transcripts(transcripts.TranscriptConfig(enabled=True, root=work / "store" / "transcripts"))
    t0 = time.monotonic()
    idx = memory_index.MemoryIndex(work / "seed" / memory_index.DB_FILE, tr, embed)
    idx.ensure_built()
    stats = idx.stats()
    with idx._lock:
        idx._db.executescript("DELETE FROM fts; DELETE FROM units; DELETE FROM days;")
        idx._db.execute("VACUUM")
    idx.close()
    manifest = {"key": key, "units": stats["units"], "vectors": stats["vectors"], "days": stats["days"],
                "secs": round(time.monotonic() - t0, 1), "usd": round(_spend_usd(spend_dir), 5)}
    if stats["vectors"] == 0:
        raise SystemExit("SEED_FAILED: no round was embedded")
    (work / "seed" / BUILD_FILE).write_text(json.dumps(manifest, indent=1) + "\n", encoding="utf-8")
    try:
        os.rename(work / "seed", final)
    except OSError:
        pass                                                # another run finished first: use its seed
    shutil.rmtree(work, ignore_errors=True)
    return final / memory_index.DB_FILE, json.loads((final / BUILD_FILE).read_text(encoding="utf-8"))


def seed_index(folder: Path, seed: Optional[Path]) -> None:
    """The hybrid seed into one question's fresh memory copy, before it opens."""
    from eve_bridge import memory_index

    if seed is None:
        return
    dst = memory_index.index_path(assert_temp(folder))
    dst.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(seed, dst)


# ---- the brain --------------------------------------------------------------------------------------------------

class _NoApi:
    """The Telegram bot API, lent to nothing: every call is a bug in the eval."""

    def __getattr__(self, name: str) -> Any:
        raise RuntimeError(f"the memory eval never talks to Telegram ({name})")


def daemon_environment(memory_dir: Path, spend_dir: Path) -> None:
    """The daemon's environment (the plist's keys, then the owner's env file, filling gaps), with memory on in
    the eval's temp folder and the spend rows in the run's folder. The persona's location is the data's, so the
    owner's configured location is dropped. Values are never printed."""
    from eve_bridge.envfile import load_env_file

    keep = {k: os.environ[k] for k in DAEMON_ENV if k in os.environ}
    os.environ.clear()
    os.environ.update(keep)
    load_env_file()
    os.environ["EVE_MEMORY"] = "1"
    os.environ["EVE_MEMORY_DIR"] = str(assert_temp(memory_dir))
    os.environ["EVE_SPEND_DIR"] = str(spend_dir)
    os.environ.pop("EVE_LOCATION", None)


async def _no_owner_to_ask(name: str, args: Any, chat_id: int) -> None:
    """The taint gate (tool_gate.py) with no owner to ask: the call goes on to the eval's own _tool, which never
    runs an acting tool, so the call is recorded as the model made it."""
    return None


def make_inlet(cfg: Any, create: Any, mem: Any, clock: Clock) -> Any:
    from eve_bridge import recall
    from eve_bridge import telegram as tg

    return tg.TelegramInlet(cfg, _NoApi(), create, memory=mem, toolset_mode="off",
                            brief=lambda: recall.opening_brief(mem.cfg, mem.transcripts, now=clock.now))


def pin_clock(clock: Clock) -> None:
    """The turn's system context reads the eval's clock (telegram.turn_context calls system_context.context)."""
    from eve_bridge import system_context

    real = getattr(system_context.context, "__wrapped__", system_context.context)

    def context(environ: Any = None, *, now: Optional[datetime] = None) -> str:
        return real(environ, now=now or clock.now)

    context.__wrapped__ = real  # type: ignore[attr-defined]
    system_context.context = context


@dataclass
class Turn:
    reply: str = ""
    rounds: int = 0
    calls: list[dict[str, Any]] = field(default_factory=list)
    usage: dict[str, int] = field(default_factory=lambda: {"in": 0, "cached": 0, "out": 0, "instr": 0})
    instructions: str = ""
    secs: float = 0.0
    error: str = ""


async def ask(inlet: Any, question: str, create: Any) -> Turn:
    """One question as a fresh chat's first message, through TelegramInlet._think, with the eval's note after it."""
    from eve_bridge import telegram as tg

    turn = Turn()
    memory_names = {t["name"] for t in inlet._memory_tools()}

    async def tool(name: str, args: dict[str, Any], chat_id: int) -> dict[str, Any]:
        t0 = time.monotonic()
        if name in memory_names and name != "forget_apply":
            result = await tg.TelegramInlet._memory_tool(inlet, name, args)
        else:
            result = dict(UNAVAILABLE)
        turn.calls.append({"name": name, "args": args, "ms": round((time.monotonic() - t0) * 1000, 1),
                           "result": result})
        return result

    async def recording(payload: dict[str, Any]) -> dict[str, Any]:
        if not turn.instructions:
            turn.instructions = str(payload.get("instructions") or "")
        return await create(payload)

    inlet._tool = tool
    inlet._gate = _no_owner_to_ask
    inlet._create = recording
    items = [tg.message_item("user", question), tg.message_item("developer", EVAL_NOTE)]
    t0 = time.monotonic()
    try:
        turn.reply, turn.rounds = await inlet._think(items, 0, tail=0, usage=turn.usage)
    except Exception as e:  # noqa: BLE001 - the type only: the question scores 0 as an error
        turn.error = type(e).__name__
    turn.secs = round(time.monotonic() - t0, 2)
    return turn


def retrying(real: Any) -> Any:
    async def create(payload: dict[str, Any]) -> dict[str, Any]:
        last: Optional[BaseException] = None
        for attempt in range(ATTEMPTS):
            try:
                return await real(payload)
            except Exception as e:  # noqa: BLE001
                last = e
                await asyncio.sleep(2.0 * (attempt + 1))
        assert last is not None
        raise last
    return create


# ---- one row, and the summary -----------------------------------------------------------------------------------

def make_row(q: Mapping[str, Any], split: str, turn: Turn, evidence: Sequence[str], unresolved: int,
             stored: str, forget: Optional[dict[str, Any]] = None) -> dict[str, Any]:
    """Ids and scores only: never the question, the answer or a tool's words."""
    reply = parse_reply(turn.reply) if not turn.error else None
    s = score(q, reply)
    year = asked_year(q)
    results = [c["result"] for c in turn.calls]
    keys = q.get("answer_keys") or []
    abstain_gold = bool(q.get("abstain"))
    res_text = "\n".join(s2 for r in results for s2 in strings(r))
    in_results = bool(keys) and keys_present(keys, res_text, year)
    in_prompt = bool(keys) and keys_present(keys, turn.instructions, year)
    is_stored = bool(keys) and keys_present(keys, stored, year)
    if turn.error:
        failure = "error"
    elif reply is None:
        failure = "format"
    elif s:
        failure = ""
    elif abstain_gold and reply.get("abstain") is True:
        failure = "forget_leak"                          # abstained, but the answer still held a leak key
    elif abstain_gold:
        failure = "answered_instead_of_abstaining"
    elif in_results or in_prompt:
        failure = "answer"
    elif is_stored:
        failure = "retrieval"
    else:
        failure = "storage"
    searches = [c["ms"] for c in turn.calls if c["name"] == "memory_search"]
    row = {"id": q.get("id"), "category": q.get("category"), "split": split, "score": s,
           "abstain_gold": abstain_gold, "abstain_pred": None if reply is None else bool(reply["abstain"]),
           "json_ok": reply is not None, "evidence": len(evidence), "evidence_unresolved": unresolved,
           "evidence_hit": evidence_hit(evidence, results) if evidence else None,
           "evidence_in_prompt": evidence_hit(evidence, [turn.instructions]) if evidence else None,
           "keys_in_results": in_results, "keys_in_prompt": in_prompt, "stored": is_stored, "failure": failure,
           "rounds": turn.rounds, "tools": [c["name"] for c in turn.calls], "memory_search_ms": searches,
           "tokens": dict(turn.usage), "secs": turn.secs, "error": turn.error}
    if forget is not None:
        row["forget"] = {k: forget[k] for k in ("ok", "previewed", "forgot", "queries") if k in forget}
        # A forgotten needle came back: a leak key in the answer, or the forgotten line itself reached the brain
        # (prompt or tool result).
        leak = reply is not None and leaked(q, str(reply.get("answer") or ""))
        leak = leak or (bool(evidence) and (evidence_hit(evidence, results)
                                            or evidence_hit(evidence, [turn.instructions])))
        row["forget_leak"] = leak
        row["forget_ok"] = bool(forget.get("ok")) and s == 1 and not leak
    return row


def p95(values: Sequence[float]) -> Optional[float]:
    if not values:
        return None
    v = sorted(values)
    return v[max(0, math.ceil(0.95 * len(v)) - 1)]


def summarize(rows: Sequence[Mapping[str, Any]]) -> dict[str, Any]:
    n = len(rows)
    cats: dict[str, list[int]] = {}
    for r in rows:
        cats.setdefault(str(r["category"]), []).append(int(r["score"]))
    ev = [r for r in rows if r.get("evidence_hit") is not None and "forget_ok" not in r]   # a forget hit is a leak
    pred = [r for r in rows if r.get("abstain_pred") is True]
    gold = [r for r in rows if r["abstain_gold"]]
    tp = sum(1 for r in pred if r["abstain_gold"])
    lat = [ms for r in rows for ms in r["memory_search_ms"]]
    fails: dict[str, int] = {}
    for r in rows:
        if r["failure"]:
            fails[r["failure"]] = fails.get(r["failure"], 0) + 1
    forgets = [r for r in rows if "forget_ok" in r]

    def mean(key: str) -> float:
        return round(sum(r["tokens"].get(key, 0) for r in rows) / n, 1) if n else 0.0

    return {
        "n": n, "accuracy": round(sum(r["score"] for r in rows) / n, 4) if n else None,
        "by_category": {c: {"n": len(v), "accuracy": round(sum(v) / len(v), 4)} for c, v in sorted(cats.items())},
        "retrieval_hit_rate": round(sum(1 for r in ev if r["evidence_hit"]) / len(ev), 4) if ev else None,
        "retrieval_n": len(ev),
        "evidence_unresolved": sum(r["evidence_unresolved"] for r in rows),
        "keys_in_results_rate": round(sum(1 for r in rows if r["keys_in_results"]) / n, 4) if n else None,
        "abstention_precision": round(tp / len(pred), 4) if pred else None,
        "abstention_recall": round(tp / len(gold), 4) if gold else None,
        "forget_compliance": round(sum(1 for r in forgets if r["forget_ok"]) / len(forgets), 4) if forgets else None,
        "forget_n": len(forgets),
        "memory_search_p95_ms": p95(lat), "memory_search_calls": len(lat),
        "tokens_per_turn": {"in": mean("in"), "cached": mean("cached"), "out": mean("out"),
                            "instructions_chars": mean("instr")},
        "failures": dict(sorted(fails.items())),
        "json_failures": sum(1 for r in rows if not r["json_ok"] and not r["error"]),
        "errors": sum(1 for r in rows if r["error"]),
    }


def print_summary(s: Mapping[str, Any]) -> None:
    def pct(x: Any) -> str:
        return "-" if x is None else f"{100 * x:.1f}%"

    print(f"== {s['n']} questions: accuracy {pct(s['accuracy'])}")
    for c, v in s["by_category"].items():
        print(f"   {c:<24} {v['n']:>4}  {pct(v['accuracy'])}")
    print(f"   retrieval hit rate {pct(s['retrieval_hit_rate'])} of {s['retrieval_n']} (evidence unresolved: "
          f"{s['evidence_unresolved']}); answer keys in tool results {pct(s['keys_in_results_rate'])}")
    print(f"   abstention precision {pct(s['abstention_precision'])}, recall {pct(s['abstention_recall'])}")
    print(f"   forget compliance {pct(s['forget_compliance'])} of {s['forget_n']}")
    p = s["memory_search_p95_ms"]
    print(f"   memory_search p95 {'-' if p is None else f'{p:.0f} ms'} over {s['memory_search_calls']} calls")
    t = s["tokens_per_turn"]
    print(f"   tokens per turn: in {t['in']:.0f} (cached {t['cached']:.0f}), out {t['out']:.0f}; instructions "
          f"{t['instructions_chars']:.0f} chars")
    print(f"   failures {s['failures']}; json failures {s['json_failures']}; errors {s['errors']}")


# ---- the run --------------------------------------------------------------------------------------------------

def run(ds: Dataset, questions: Sequence[dict[str, Any]], split: str, built: Path, run_root: Path,
        trace_path: Path, seed: Optional[Path] = None) -> list[dict[str, Any]]:
    from eve_bridge import telegram as tg
    from eve_bridge.computer_agent import make_response_creator

    tdir = ds.root / TRANSCRIPTS
    tz = data_tz(tdir, ds.days)
    clock = Clock()
    pin_clock(clock)
    tg.MAX_TOOL_ROUNDS = MAX_ROUNDS
    cfg = tg.configured()
    create = retrying(make_response_creator())
    rows: list[dict[str, Any]] = []

    async def go() -> None:
        with open(trace_path, "w", encoding="utf-8") as trace:
            for i, q in enumerate(questions):
                clock.now = asked_at(q, tz)
                evidence, unresolved = evidence_texts(q, tdir)
                folder, snap = memory_as_of(built, tdir, ds.days, clock.now, run_root / f"q-{i}")
                seed_index(folder, seed)
                mem = open_memory(folder, clock)
                ranked = getattr(mem, "ranked", None)
                if ranked is not None:
                    await asyncio.to_thread(ranked.ensure_built)
                fqs = forget_queries(q, tdir)
                forget = None
                if fqs is not None:
                    resolved = q.get("forget") is not None and (
                        not isinstance(q.get("forget"), Mapping) or "query" in q["forget"]
                        or ref_text(tdir, q["forget"]) is not None)
                    forget = await asyncio.to_thread(run_forgets, mem, fqs, resolved)
                stored = stored_text(mem)
                brain = make_inlet(cfg, create, mem, clock)
                turn = await ask(brain, str(q.get("question") or ""), create)
                row = make_row(q, split, turn, evidence, unresolved, stored, forget)
                row["memory_as_of"] = snap
                rows.append(row)
                trace.write(json.dumps({"id": q.get("id"), "asked_at": clock.now.isoformat(), "memory_as_of": snap,
                                        "forget": forget, "reply": turn.reply, "calls": turn.calls,
                                        "usage": turn.usage, "error": turn.error},
                                       ensure_ascii=False, default=str) + "\n")
                trace.flush()
                print(f"   {i + 1:>4}/{len(questions)} {row['id']} [{row['category']}] score {row['score']} "
                      f"{row['failure'] or 'ok'} ({row['secs']:.1f} s)")
                if mem.index is not None:
                    _close_index(mem.index)
                if ranked is not None:
                    ranked.close()
                shutil.rmtree(folder, ignore_errors=True)

    asyncio.run(go())
    return rows


def main(argv: Optional[list[str]] = None) -> int:
    p = argparse.ArgumentParser(prog="memory_eval", description=__doc__.split("\n\n")[0])
    p.add_argument("--data", default=str(DATA_DIR), help="the folder with persona.json, transcripts/, questions.json")
    p.add_argument("--estimate", action="store_true", help="print cost and time, call nothing")
    p.add_argument("--split", choices=("dev", "holdout"), default="dev")
    p.add_argument("--limit", type=int, default=None)
    p.add_argument("--out", default=None, help="write meta, summary and rows (ids and scores only) here")
    p.add_argument("--trace", default=None, help="the tool-call trace (default: the run's temp folder)")
    p.add_argument("--seal-check", action="store_true", help="check questions.json against questions.sha256")
    p.add_argument("--cache-dir", default=str(Path(tempfile.gettempdir()) / CACHE_NAME))
    p.add_argument("--max-build-usd", type=float, default=DEFAULT_MAX_BUILD_USD)
    p.add_argument("--features", default="", help="memory switches on for this run: hybrid, timeargs, retry, "
                   "validity, sources (comma-separated) or all; every other switch is off")
    args = p.parse_args(argv)
    features = parse_features(args.features)

    data = Path(args.data)
    ds = load_dataset(data)
    if args.seal_check or args.split == "holdout":
        ok, digest, sealed = seal_check(data)
        print(f"   seal: questions.json sha256 {digest} {'=' if ok else '!='} {SEAL} {sealed or '(missing)'}")
        if args.seal_check and not ok:
            print("SEAL_MISMATCH")
            return 2
        if args.split == "holdout" and not ok:
            print("SEAL_MISMATCH: the holdout runs only while questions.json matches its seal")
            return 2
        if args.seal_check and not args.estimate and args.out is None and args.split != "holdout":
            print("SEAL_OK")
            return 0

    questions = select(ds, args.split, args.limit)
    counts = {s: len(select(ds, s)) for s in ("dev", "holdout")}
    print(f"   data {data_sha(data)[:16]}: {len(ds.days)} days; questions dev {counts['dev']}, holdout "
          f"{counts['holdout']}; this run {args.split} {len(questions)}")
    cache_root = Path(args.cache_dir)
    assert_temp(cache_root / "x")
    run_root = assert_temp(Path(tempfile.mkdtemp(prefix="memory-eval-run-")))
    daemon_environment(run_root / "env-memory", run_root / "spend")
    set_features(features)
    print(f"   features: {', '.join(features) or 'none'}")

    from eve_bridge import mem0_memory
    from eve_bridge import telegram as tg
    from eve_bridge.recall import RecallConfig

    cfg = tg.configured()
    key = build_key(data)
    cached = (cache_root / key / BUILD_FILE).is_file()
    probe = RecallConfig(store=run_root / "env-memory", notes=run_root / "notes")
    index_on = mem0_memory.configured(probe).enabled and mem0_memory.available()
    est = estimate(ds, questions, ask_model=cfg.model, about_chars=len(cfg.about), index_on=index_on,
                   cached=cached)
    print_estimate(est)
    if args.estimate:
        shutil.rmtree(run_root, ignore_errors=True)
        print("ESTIMATE_ONLY")
        return 0
    if not cached and est["build_usd"] > args.max_build_usd:
        print(f"BUILD_REFUSED: the estimate ${est['build_usd']:.2f} is over --max-build-usd {args.max_build_usd}")
        return 2

    built, manifest = build_memory(ds, cache_root)
    print(f"   memory {built.name}: {manifest['dreamt']}/{manifest['days']} days dreamt, failed "
          f"{len(manifest['failed_days'])}, mem0 {'on' if manifest['index'] else 'off'}, build ${manifest['usd']:.4f}")
    seed, seeded = None, None
    if "hybrid" in features:
        seed, seeded = hybrid_seed(ds, cache_root)
        print(f"   hybrid seed {seeded['key']}: {seeded['vectors']} vectors for {seeded['units']} rounds, "
              f"${seeded['usd']:.5f}")
    os.environ["EVE_SPEND_DIR"] = str(run_root / "spend")
    trace = Path(args.trace) if args.trace else run_root / "trace.jsonl"
    t0 = time.monotonic()
    rows = run(ds, questions, args.split, built, run_root, trace, seed)
    summary = summarize(rows)
    usd = _spend_usd(run_root / "spend")
    print_summary(summary)
    print(f"   ask cost ${usd:.4f} in {time.monotonic() - t0:.0f} s; trace {trace}")
    if args.out:
        meta = {"run": datetime.now().astimezone().isoformat(timespec="seconds"), "split": args.split,
                "limit": args.limit, "data_sha256": data_sha(data), "questions_sha256": sha256_file(data / QUESTIONS),
                "build": manifest, "model": cfg.model, "effort": cfg.effort, "max_rounds": MAX_ROUNDS,
                "ask_hour": ASK_HOUR, "ask_usd": round(usd, 4), "features": features, "hybrid_seed": seeded}
        Path(args.out).write_text(json.dumps({"meta": meta, "summary": summary, "rows": rows}, indent=1) + "\n",
                                  encoding="utf-8")
        print(f"   kept in {args.out}")
    print("MEMORY_EVAL_COMPLETE")
    return 0


if __name__ == "__main__":
    sys.exit(main())
