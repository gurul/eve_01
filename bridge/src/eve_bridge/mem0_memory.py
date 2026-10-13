"""mem0, self-hosted: the meaning search over everything the owner has said to eve.

records.py answers "what do you know about me" with a page and a keyword search. It misses a question
asked in other words than the record used ("where does my sister live" against "moved to the coast").
This is the layer that catches it: mem0's open-source library (mem0ai, Apache-2.0), run on this
computer, with its vector store (Qdrant, on disk) and its history database both under
``<memory>/mem0``. Only the two model calls leave the machine: fact extraction and embeddings, both
OpenAI models (owner, 2026-09-23: "it can go to openai"), both through OpenRouter on the owner's key since
2026-10-08 (owner: "make everything use my openrouter key"; docs/openrouter.md). The embedder is the same
text-embedding-3-small at 1536 dimensions it always was, so the vectors already on disk stay comparable.

It is an INDEX, not a source (owner, 2026-09-23: three stores — transcripts, records, mem0). The nightly
dream feeds it each finished conversation of the day, natively: what the owner said goes in as the user's
messages and what eve said as the assistant's, dated, with its channel, so mem0 extracts the owner's
own facts rather than facts about a note. Tool calls, commands and relays never go in. It can be rebuilt
from the transcripts at any time (``memory reindex``), which is why a failure here is never a loss.

The brains never write it: they search it (``search``, dated hits, 3 s at most) through the Memory
facade. A forget reaches it (``find`` + ``forget``), including the rows mem0's own history database keeps.

Privacy, as code rather than hope:

- mem0 ships PostHog telemetry, on by default and read at import time. ``MEM0_TELEMETRY`` is forced off
  before the first import, and the import is refused if telemetry still reads on.
- mem0 writes ``~/.mem0/config.json`` at import. ``MEM0_DIR`` points it inside eve's own folder.
- The folder is 0700 and its files 0600, set again every time it opens; nothing here is in git.

Failure is silence: no key, no network, no mem0 installed — search returns nothing, a conversation that
failed to go in is tried again next night, and nothing about a conversation breaks. A failure to open is
retried after ten minutes, so one bad moment (a key not yet set, a lock held) does not last until restart.
"""

from __future__ import annotations

import contextlib
import importlib.util
import logging
import os
import re
import sqlite3
import threading
import time
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import Any, Callable, Optional

from . import spend
from . import transcripts as transcripts_mod
from .recall import RecallConfig

log = logging.getLogger(__name__)

DEFAULT_MODEL = "gpt-6-luna"
EXTRACT_MAX_OUTPUT_TOKENS = 2000
DEFAULT_EMBED_MODEL = "text-embedding-3-small"
EMBED_DIMS = 1536                              # text-embedding-3-small
# EVE_MEM0_EMBEDDER: "openrouter" (the default, everything above) or "gemma" (an experiment, 2026-10-09:
# EmbeddingGemma on this Mac, so a memory search sends nothing out; tools/evals/embeddinggemma/VERDICT.md).
# google/embeddinggemma-300m is gated on Hugging Face (a license click per account); unsloth's copy is not, and
# its configs are byte-identical to Google's (same git blob ids) and its weights the same sizes. It is pinned to
# one revision and only ever read from the local Hugging Face cache: the daemon never downloads it. Gemma Terms
# of Use apply either way.
EMBEDDERS = ("openrouter", "gemma")
GEMMA_MODEL = "unsloth/embeddinggemma-300m"
GEMMA_REVISION = "bfa3c846ac738e62aa61806ef9112d34acb1dc5a"
GEMMA_DIMS = 768                               # its full width (the card: Matryoshka to 512/256/128 also)
GEMMA_QUERY = "query"                          # its prompts: "task: search result | query: "
GEMMA_DOCUMENT = "document"                    # and "title: none | text: "
GEMMA_HOME = "gemma"                           # <mem0>/gemma: its own store, ledger and history; 1536 never touched
OWNER = "owner"                                # the one user id: this is one person's memory
INGESTED_CONVS = "ingested_convs"              # one conversation id per line: what mem0 has read
HISTORY_DB = "history.db"
MAX_HITS = 5
MIN_SCORE = 0.25                               # below this a hit is a guess, not a memory
SEARCH_TIMEOUT_SECS = 3.0                      # a hung embedding call must not stall a turn
MAX_SEARCHES_IN_FLIGHT = 2                     # abandoned searches still running: past this, answer [] at once
RETRY_AFTER_SECS = 600.0                       # a failed open is tried again after ten minutes
TIMEOUT_LOG_EVERY_SECS = 3600.0
CHUNK_MESSAGES = 40                            # messages per mem0.add call
MESSAGE_CHARS = 1500                           # one message, clipped
GET_ALL_CAP = 10000
STAR = "★"                                     # a candidate eve proposed, never the owner's own fact

# mem0's extraction prompt addition (MemoryConfig.custom_instructions, verified in mem0ai 2.2.0).
CUSTOM_INSTRUCTIONS = (
    "The user is the owner of eve, a personal sidekick on their Mac, and the assistant is eve. Extract facts about "
    "the owner only: their life, work, people, plans, preferences and routines, as the owner said them. "
    "A request to operate the robot or its tools (move, look, play a sound, set a timer, send a message, run "
    "a command) is not a fact about the owner: extract nothing from it. Never extract what eve said about "
    "itself, and never a guess eve made about the owner that the owner did not confirm.")

_DAY = re.compile(r"(\d{4}-\d{2}-\d{2})")


@dataclass(frozen=True)
class Mem0Config:
    enabled: bool = False
    home: Path = Path("~/.config/eve-bridge/memory/mem0").expanduser()
    model: str = DEFAULT_MODEL
    embed_model: str = DEFAULT_EMBED_MODEL
    embedder: str = "openrouter"


def embedder_of(environ: Any = None) -> str:
    """``EVE_MEM0_EMBEDDER``: "gemma" for the local EmbeddingGemma, anything else (unset too) "openrouter"."""
    env = os.environ if environ is None else environ
    name = str(env.get("EVE_MEM0_EMBEDDER") or "").strip().lower()
    return name if name in EMBEDDERS else "openrouter"


def configured(recall_cfg: RecallConfig, environ: Any = None) -> Mem0Config:
    """On with memory (``EVE_MEMORY=1``, or the older ``EVE_RECORDS=1``); ``EVE_MEM0=0``
    turns just this index off. Its home is the memory root's ``mem0/``; with ``EVE_MEM0_EMBEDDER=gemma`` it is
    ``mem0/gemma/``, a store of its own (768 dimensions cannot share the 1536 one)."""
    env = os.environ if environ is None else environ
    memory_on = transcripts_mod.configured(env).enabled
    index_off = str(env.get("EVE_MEM0") or "").strip().lower() in ("0", "false", "no", "off")
    enabled = memory_on and not index_off
    if embedder_of(env) == "gemma":
        return Mem0Config(enabled=enabled, home=recall_cfg.mem0_dir / GEMMA_HOME, embed_model=GEMMA_MODEL,
                          embedder="gemma")
    return Mem0Config(enabled=enabled, home=recall_cfg.mem0_dir)


def available() -> bool:
    """Whether mem0ai is installed. Checked without importing it (importing starts its telemetry)."""
    try:
        return importlib.util.find_spec("mem0") is not None
    except (ImportError, ValueError):
        return False


# mem0 builds its own openai clients in Memory.from_config and refuses to without a key. They are replaced at once
# (``on_openrouter``) and never send a request, so they get this placeholder, never a real key: no key is ever in
# mem0's config.
PLACEHOLDER_KEY = "replaced-by-eve"


def mem0_config(cfg: Mem0Config) -> dict[str, Any]:
    """The Memory.from_config dict: mem0's openai provider for the two model calls (its clients are swapped for
    OpenRouter's by ``on_openrouter``), everything stored under ``cfg.home``. With the gemma embedder, mem0's
    huggingface provider loads EmbeddingGemma from the local cache (``on_gemma`` adds its task prompts)."""
    # mem0's name heuristic predates Luna: without the explicit flag it silently drops
    # reasoning_effort and sends sampling settings meant for non-reasoning models.
    llm = {"provider": "openai", "config": {
        "model": cfg.model, "api_key": PLACEHOLDER_KEY, "reasoning_effort": "low",
        "is_reasoning_model": True, "max_tokens": EXTRACT_MAX_OUTPUT_TOKENS}}
    if cfg.embedder == "gemma":
        return {
            "llm": llm,
            "embedder": {"provider": "huggingface", "config": {
                "model": GEMMA_MODEL, "embedding_dims": GEMMA_DIMS,
                # CPU, not the default MPS: on the GPU fp32 weights held 1.2 GB of unified
                # memory and a query took 151 ms; on CPU the process is 1.24 GB smaller and a
                # query takes ~20 ms (measured 2026-10-13).
                "model_kwargs": {"revision": GEMMA_REVISION, "local_files_only": True, "device": "cpu"}}},
            "vector_store": {"provider": "qdrant", "config": {
                "collection_name": "eve_gemma", "path": str(cfg.home / "qdrant"), "on_disk": True,
                "embedding_model_dims": GEMMA_DIMS}},
            "history_db_path": str(cfg.home / HISTORY_DB),
            "custom_instructions": CUSTOM_INSTRUCTIONS,
        }
    return {
        "llm": llm,
        "embedder": {"provider": "openai", "config": {"model": cfg.embed_model, "api_key": PLACEHOLDER_KEY}},
        "vector_store": {"provider": "qdrant", "config": {
            "collection_name": "eve", "path": str(cfg.home / "qdrant"), "on_disk": True,
            "embedding_model_dims": EMBED_DIMS}},
        "history_db_path": str(cfg.home / HISTORY_DB),
        "custom_instructions": CUSTOM_INSTRUCTIONS,
    }


def make_private(home: Path) -> None:
    """``home`` and every folder under it 0700, every file 0600. Symlinks are never followed."""
    with contextlib.suppress(OSError):
        home.mkdir(parents=True, exist_ok=True)
    if home.is_symlink():
        return
    with contextlib.suppress(OSError):
        os.chmod(home, 0o700)
    for root, dirs, files in os.walk(home):
        for name in dirs:
            p = os.path.join(root, name)
            if not os.path.islink(p):
                with contextlib.suppress(OSError):
                    os.chmod(p, 0o700)
        for name in files:
            p = os.path.join(root, name)
            if not os.path.islink(p):
                with contextlib.suppress(OSError):
                    os.chmod(p, 0o600)


def open_mem0(cfg: Mem0Config) -> Any:
    """The real mem0 Memory, with telemetry off and its home inside ours. Raises when that cannot be had."""
    cfg.home.mkdir(parents=True, exist_ok=True)
    os.environ["MEM0_TELEMETRY"] = "False"          # read once, at import: must be set before it
    os.environ["MEM0_DIR"] = str(cfg.home / "home")  # where mem0 keeps its config.json, instead of ~/.mem0
    from mem0 import Memory
    from mem0.memory import telemetry

    if telemetry.MEM0_TELEMETRY:
        raise RuntimeError("mem0 telemetry is on (mem0 was imported before eve could turn it off)")
    if cfg.embedder == "gemma":
        os.environ.setdefault("HF_HUB_DISABLE_TELEMETRY", "1")  # the model itself: local_files_only, never fetched
        return meter(on_gemma(on_openrouter(Memory.from_config(mem0_config(cfg)), embeddings=False)), cfg)
    return meter(on_openrouter(Memory.from_config(mem0_config(cfg))), cfg)


def on_openrouter(memory: Any, embeddings: bool = True) -> Any:
    """mem0's two clients, fact extraction (``memory.llm.client``) and embeddings (``memory.embedding_model.client``),
    replaced with OpenRouter's (openrouter.client: the owner's OPENROUTER_API_KEY, and "gpt-6-luna" sent as
    "openai/gpt-6-luna"). Until 2026-10-08 this pinned both to OpenAI's own endpoint on OPENAI_API_KEY; the owner
    moved every model call to his OpenRouter key. mem0ai 2.2 checks OPENROUTER_API_KEY in the environment on each
    extraction call and then sends no ``store`` field, which is what OpenRouter wants. Raises without a key.
    ``embeddings=False`` (the gemma embedder) moves extraction only: the embeddings stay on this Mac."""
    from . import openrouter

    try:
        memory.llm.client = openrouter.client()
        # mem0 2.2 drops its configured output limit for reasoning models. Restore the
        # extraction budget on this instance, before the spend wrapper observes calls.
        completions = memory.llm.client.chat.completions
        create = completions.create

        def bounded_create(*args: Any, **kwargs: Any) -> Any:
            if "max_completion_tokens" not in kwargs and "max_tokens" not in kwargs:
                kwargs["max_completion_tokens"] = EXTRACT_MAX_OUTPUT_TOKENS
            return create(*args, **kwargs)

        completions.create = bounded_create
        if embeddings:
            memory.embedding_model.client = openrouter.client()
    except AttributeError as e:
        raise RuntimeError("mem0: its model clients cannot be moved to OpenRouter") from e
    return memory


def on_gemma(embedder: Any) -> Any:
    """EmbeddingGemma with the task prompts its card asks for. mem0 2.2's huggingface provider encodes with no
    prompt; here a search (``memory_action="search"``) is encoded as a query and everything mem0 stores (``add``,
    ``update``) as a document, vectors normalised. Takes the Memory, or mem0's embedder itself (the eval)."""
    emb = getattr(embedder, "embedding_model", embedder)
    model = emb.model

    def encode(texts: Any, memory_action: Any) -> Any:
        prompt = GEMMA_QUERY if memory_action == "search" else GEMMA_DOCUMENT
        return model.encode(texts, prompt_name=prompt, normalize_embeddings=True, convert_to_numpy=True)

    def embed(text: str, memory_action: Any = None) -> list[float]:
        return encode(text, memory_action).tolist()

    def embed_batch(texts: list[str], memory_action: Any = "add") -> list[list[float]]:
        return encode(list(texts), memory_action).tolist() if texts else []

    emb.embed = embed
    emb.embed_batch = embed_batch
    return embedder


def meter(memory: Any, cfg: Mem0Config) -> Any:
    """Route mem0's two model calls through the daily spend meter (spend.py). mem0 owns its clients, so the
    ``create`` of its fact extraction (Chat Completions, ``memory.llm.client``) and of its embeddings
    (``memory.embedding_model.client``) is wrapped on those instances: each reply is recorded, then returned
    unchanged. A client shaped otherwise (another mem0 version) is left as it is, with one log line: the index
    must never break for want of a meter."""
    def wrap(owner: Any, record: Any, what: str) -> None:
        original = getattr(owner, "create", None)
        if not callable(original) or getattr(original, "_eve_metered", False):
            log.warning("mem0: %s calls are not metered (no create to wrap)", what)
            return

        def create(*args: Any, **kwargs: Any) -> Any:
            reply = original(*args, **kwargs)
            record(reply, str(kwargs.get("model") or ""))
            return reply

        create._eve_metered = True  # type: ignore[attr-defined]
        owner.create = create

    try:
        wrap(memory.llm.client.chat.completions,
             lambda r, m: spend.record_chat_completion(spend.MEMORY, r, model=m or cfg.model),
             "extraction")
        if cfg.embedder != "gemma":                # local embeddings cost nothing and have no client
            wrap(memory.embedding_model.client.embeddings,
                 lambda r, m: spend.record_embedding(spend.MEMORY, m or cfg.embed_model, r), "embedding")
    except AttributeError:
        log.warning("mem0: its model calls are not metered (this mem0 has no client where eve looks)")
    return memory


def _rows(found: Any) -> list[dict[str, Any]]:
    rows = found.get("results", []) if isinstance(found, dict) else (found or [])
    return [r for r in rows if isinstance(r, dict)]


def _norm(text: str) -> str:
    return " ".join(str(text or "").split()).strip(" .,;:!").casefold()


def _src_of(row: dict[str, Any]) -> str:
    meta = row.get("metadata") if isinstance(row.get("metadata"), dict) else {}
    return " ".join(str(meta.get("src") or "").split())


def _match_text(row: dict[str, Any]) -> str:
    """What a forget's match reads for one memory: its text, and each of its srcs the way a record line carries
    one (the owner line's, and eve's line before it when the unit held one), so a forget that follows a redacted
    line's pointer (memory.Memory._follow_sources) reaches it, whichever of the two lines was forgotten."""
    meta = row.get("metadata") if isinstance(row.get("metadata"), dict) else {}
    srcs = [s for s in (_src_of(row), " ".join(str(meta.get("src_before") or "").split())) if s]
    return " ".join([str(row.get("memory") or ""), *(f"{{src: {s}}}" for s in srcs)])


def _sources_on() -> bool:
    from .records import sources_on

    return sources_on()


def _stamp(ts: Any) -> str:
    """A transcript line's ts → "YYYY-MM-DD HH:MM", its own date and time (the src form). "" when unreadable."""
    try:
        return datetime.fromisoformat(str(ts)).strftime("%Y-%m-%d %H:%M")
    except (TypeError, ValueError):
        return ""


def _day_of(row: dict[str, Any]) -> str:
    """The day a memory is from: its own `day`, else the date in a legacy note path, else when mem0 made it."""
    meta = row.get("metadata") if isinstance(row.get("metadata"), dict) else {}
    for raw in (meta.get("day"), meta.get("note"), row.get("created_at")):
        m = _DAY.search(str(raw or ""))
        if m:
            return m.group(1)
    return ""


class OwnerMemory:
    """mem0 for one owner. ``factory`` makes the Memory (open_mem0 by default; tests pass a fake);
    ``clock`` is monotonic seconds (tests inject one).

    Every call to mem0 is serialised (a local Qdrant store is one writer at a time). Search runs on a
    daemon thread of its own so it can be abandoned after SEARCH_TIMEOUT_SECS without holding up the
    process's exit; the rest is called from the dream's worker thread."""

    def __init__(self, cfg: Mem0Config, factory: Optional[Callable[[Mem0Config], Any]] = None, *,
                 clock: Callable[[], float] = time.monotonic) -> None:
        self.cfg = cfg
        self.home = cfg.home
        self._factory = factory or open_mem0
        self._clock = clock
        self._mem: Any = None
        self._failed_at: Optional[float] = None
        self._lock = threading.Lock()
        self._forgets = 0                    # forgets so far; an ingest that saw it move adds nothing more
        self._in_flight = 0
        self._count_lock = threading.Lock()
        self._last_timeout_log = -TIMEOUT_LOG_EVERY_SECS

    # -- opening -------------------------------------------------------------------------------

    def _memory(self) -> Any:
        """The Memory, opened on first use. Call with self._lock held. None while off."""
        if self._mem is not None:
            return self._mem
        if self._failed_at is not None and self._clock() - self._failed_at < RETRY_AFTER_SECS:
            return None
        try:
            make_private(self.home)
            self._mem = self._factory(self.cfg)
            make_private(self.home)          # what opening just created, too
            self._failed_at = None
            log.info("mem0: on — %s extracts, %s embeds, stored in %s", self.cfg.model,
                     self.cfg.embed_model, self.home)
        except Exception as e:  # noqa: BLE001 - not installed, no key, telemetry refused: all are "off"
            self._failed_at = self._clock()
            log.warning("mem0: unavailable (%s) — tried again in %d minutes", type(e).__name__,
                        int(RETRY_AFTER_SECS // 60))
        return self._mem

    def warm(self) -> None:
        """Open the store and embed one word, on the caller's thread (the daemon calls it on one of its own).
        With the local embedder the first open loads the model (~3 s), longer than a search's SEARCH_TIMEOUT_SECS
        cap, so the first search after a start answered without mem0 (live, 2026-10-09). Never raises."""
        try:
            with self._lock:
                mem = self._memory()
            if mem is not None:
                embed = getattr(getattr(mem, "embedding_model", None), "embed", None)
                if embed is not None:
                    embed("warm", "search")
                log.info("mem0: warm")
        except Exception as e:  # noqa: BLE001 - a failed warm-up costs the first search its speed, nothing else
            log.warning("mem0: warm-up failed (%s)", type(e).__name__)

    # -- reading -------------------------------------------------------------------------------

    def _search_now(self, query: str, limit: int) -> list[dict[str, Any]]:
        with self._lock:
            mem = self._memory()
            if mem is None:
                return []
            found = mem.search(query, filters={"user_id": OWNER}, top_k=limit)
        out: list[dict[str, Any]] = []
        seen: set[str] = set()
        for r in _rows(found):
            text = " ".join(str(r.get("memory") or "").split())
            try:
                score = float(r.get("score") or 0)
            except (TypeError, ValueError):
                score = 0.0
            if not text or score < MIN_SCORE or STAR in text or _norm(text) in seen:
                continue
            seen.add(_norm(text))
            meta = r.get("metadata") if isinstance(r.get("metadata"), dict) else {}
            hit = {"text": text, "day": _day_of(r), "channel": str(meta.get("channel") or "")}
            if _src_of(r):
                hit["src"] = _src_of(r)
            out.append(hit)
        return out[:limit]

    def search(self, query: str, limit: int = MAX_HITS) -> list[dict[str, Any]]:
        """The memories that mean what `query` asks, best first: [{"text", "day", "channel"[, "src"]}], at most
        `limit`, each scoring MIN_SCORE or more. [] when there are none, mem0 is off, or it took longer than
        SEARCH_TIMEOUT_SECS (the call is abandoned, not waited for)."""
        query = " ".join(str(query or "").split())
        if not query:
            return []
        with self._count_lock:
            if self._in_flight >= MAX_SEARCHES_IN_FLIGHT:
                return []                            # earlier searches are still stuck: do not pile up threads
            self._in_flight += 1
        box: dict[str, Any] = {}
        done = threading.Event()

        def run() -> None:
            try:
                box["rows"] = self._search_now(query, max(1, min(limit, MAX_HITS)))
            except Exception as e:  # noqa: BLE001
                box["error"] = e
            finally:
                with self._count_lock:
                    self._in_flight -= 1
                done.set()

        threading.Thread(target=run, name="mem0-search", daemon=True).start()
        if not done.wait(SEARCH_TIMEOUT_SECS):
            now = self._clock()
            if now - self._last_timeout_log >= TIMEOUT_LOG_EVERY_SECS:
                self._last_timeout_log = now
                log.warning("mem0: search took over %.0f s — answered without it", SEARCH_TIMEOUT_SECS)
            return []
        if "error" in box:
            log.warning("mem0: search failed (%s)", type(box["error"]).__name__)
            return []
        return box.get("rows", [])

    def _all(self, mem: Any) -> list[dict[str, Any]]:
        return _rows(mem.get_all(filters={"user_id": OWNER}, top_k=GET_ALL_CAP))

    def find(self, match: Callable[[str], bool]) -> list[dict[str, str]]:
        """Every memory whose text `match` accepts: [{"id", "text"}]. [] when mem0 is off or failed."""
        with self._lock:
            mem = self._memory()
            if mem is None:
                return []
            try:
                rows = self._all(mem)
            except Exception as e:  # noqa: BLE001
                log.warning("mem0: listing failed (%s)", type(e).__name__)
                return []
        return [{"id": str(r["id"]), "text": str(r.get("memory") or "")}
                for r in rows if r.get("id") and match(_match_text(r))]

    # -- writing (the dream and a forget only) ----------------------------------------------------

    def _done(self) -> set[str]:
        try:
            return set((self.home / INGESTED_CONVS).read_text(encoding="utf-8").split("\n")) - {""}
        except OSError:
            return set()

    def _mark(self, conv: str) -> bool:
        try:
            fd = os.open(self.home / INGESTED_CONVS, os.O_WRONLY | os.O_CREAT | os.O_APPEND | os.O_NOFOLLOW, 0o600)
            try:
                os.write(fd, (conv + "\n").encode("utf-8"))
            finally:
                os.close(fd)
            return True
        except OSError as e:
            log.warning("mem0: could not mark a conversation read (%s)", type(e).__name__)
            return False

    @staticmethod
    def messages(lines: list[dict[str, Any]]) -> list[dict[str, str]]:
        """A conversation's lines as mem0 messages: what the owner said → user, what eve said →
        assistant. Everything else (tools, commands, relays, photos, close markers, Claude) stays out.

        A line kept as outside words (``out``: eve's reply written after a read, a task's sentence) keeps its place
        with its words replaced by transcripts.OUTSIDE_LEFT_OUT, as the dream's day_text does (round-4 review): a
        page's or a mail's words never become a memory of the owner."""
        roles = {"owner": "user", "eve": "assistant"}
        out = []
        for ln in lines:
            role = roles.get(ln.get("who", ""))
            text = " ".join(str(ln.get("text") or "").split())
            if ln.get("kind") != "say" or role is None or not text:
                continue
            if ln.get("out"):
                text = transcripts_mod.OUTSIDE_LEFT_OUT
            out.append({"role": role, "content": text[:MESSAGE_CHARS]})
        return out

    @staticmethod
    def line_units(lines: list[dict[str, Any]]) -> list[tuple[list[dict[str, str]], str, str]]:
        """A conversation as one unit per owner line, for EVE_MEMORY_SOURCES: (messages, src, src_before),
        where the messages are eve's line just before it (when it is eve's) and the owner's line, src is the
        owner line's "YYYY-MM-DD HH:MM" and src_before is eve's line's ("" without one). So every memory mem0
        makes from a unit points at both lines it may have come from."""
        units: list[tuple[list[dict[str, str]], str, str]] = []
        prev: Optional[tuple[dict[str, str], str]] = None
        for ln in lines:
            msg = OwnerMemory.messages([ln])
            if not msg:
                continue
            if msg[0]["role"] == "user":
                said = [prev[0], msg[0]] if prev is not None else [msg[0]]
                units.append((said, _stamp(ln.get("ts")), prev[1] if prev is not None else ""))
                prev = None
            else:
                prev = (msg[0], _stamp(ln.get("ts")))
        return units

    def ingest_day(self, transcripts: Any, day: str) -> int:
        """Feed each conversation of `day` that mem0 has not read to it. → how many went in.

        One ``add`` per CHUNK_MESSAGES messages, metadata {day, channel, conv}. With EVE_MEMORY_SOURCES on,
        one ``add`` per owner line instead (``line_units``), its metadata also carrying src, the line's date and
        time. A conversation is marked read only when every chunk went in; one that failed is tried again next
        time (mem0's own update step and ``tidy`` absorb a chunk read twice). A conversation in which the owner
        said nothing is marked read and not sent."""
        added = 0
        done = self._done()
        by_line = _sources_on()
        with self._lock:
            since = self._forgets            # before a word is read: a forget after it voids what was read
        for info in transcripts.conversations(day):
            if info.conv in done:
                continue
            lines = transcripts.conv_lines(info.conv)
            if by_line:
                units = self.line_units(lines)
            else:
                msgs = self.messages(lines)
                units = [(msgs[i:i + CHUNK_MESSAGES], "", "") for i in range(0, len(msgs), CHUNK_MESSAGES)]
            chunks = [u for u in units if any(m["role"] == "user" for m in u[0])]
            ok = True
            for chunk, src, src_before in chunks:
                with self._lock:
                    if self._forgets != since:
                        log.info("mem0: a forget ran while a day was read — the rest is read again next night")
                        return added
                    mem = self._memory()
                    if mem is None:
                        return added
                    try:
                        meta = {"day": day, "channel": info.ch, "conv": info.conv}
                        if src:
                            meta["src"] = src
                        if src_before:
                            meta["src_before"] = src_before
                        mem.add(chunk, user_id=OWNER, metadata=meta)
                    except Exception as e:  # noqa: BLE001
                        log.warning("mem0: a conversation did not go in (%s); tried again next time",
                                    type(e).__name__)
                        ok = False
                        break
            if not ok:
                continue
            with self._lock:
                make_private(self.home)
                if not self._mark(info.conv):
                    return added
            if chunks:
                added += 1
        if added:
            log.info("mem0: read %d conversation(s) of one day", added)
        return added

    def _delete(self, mem: Any, ids: list[str]) -> list[str]:
        gone = []
        for mid in ids:
            try:
                mem.delete(mid)
                gone.append(mid)
            except Exception as e:  # noqa: BLE001 - already gone, or mem0 failed: the rest still go
                log.warning("mem0: could not delete one memory (%s)", type(e).__name__)
        return gone

    def tidy(self) -> int:
        """Delete exact duplicates (same text, any case or spacing; the oldest stays) and anything carrying a
        ★ (a candidate eve proposed, which only the owner may promote). → how many went."""
        with self._lock:
            mem = self._memory()
            if mem is None:
                return 0
            try:
                rows = sorted(self._all(mem), key=lambda r: str(r.get("created_at") or ""))
            except Exception as e:  # noqa: BLE001
                log.warning("mem0: listing failed (%s)", type(e).__name__)
                return 0
            seen: set[str] = set()
            doomed: list[str] = []
            for r in rows:
                text, mid = str(r.get("memory") or ""), r.get("id")
                if not mid:
                    continue
                key = _norm(text)
                if STAR in text or key in seen:
                    doomed.append(str(mid))
                else:
                    seen.add(key)
            gone = self._delete(mem, doomed)
        if gone:
            log.info("mem0: tidied %d memories", len(gone))
        return len(gone)

    def forget(self, ids: list[str]) -> int:
        """Delete these memories, then everything mem0's history database still holds of them. → how many
        memories were deleted.

        mem0's ``delete`` removes the vector but writes the old text into its history table (a DELETE row),
        and every earlier ADD/UPDATE row keeps it too; its messages table keeps the last raw messages it was
        fed. So after the deletes: every history row of these ids goes, the messages table is emptied (it is
        only mem0's extraction context, and a forgotten line may be in it), with secure_delete on, and the
        databases are vacuumed so the text does not linger in free pages."""
        ids = [str(i) for i in ids if str(i or "").strip()]
        if not ids:
            return 0
        with self._lock:
            self._forgets += 1
            mem = self._memory()
            if mem is None:
                return 0
            gone = self._delete(mem, ids)
            self._scrub(ids)
        log.info("mem0: forgot %d memories", len(gone))
        return len(gone)

    def forget_matching(self, match: Callable[[str], bool]) -> int:
        """``find`` and ``forget`` in one hold of the lock, so nothing ``ingest_day`` adds can land between
        the listing and the deletes; and an ingest that read its conversation before this adds nothing
        after it. → how many memories were deleted."""
        with self._lock:
            self._forgets += 1
            mem = self._memory()
            if mem is None:
                return 0
            try:
                rows = self._all(mem)
            except Exception as e:  # noqa: BLE001
                log.warning("mem0: listing failed (%s)", type(e).__name__)
                return 0
            ids = [str(r["id"]) for r in rows if r.get("id") and match(_match_text(r))]
            gone = self._delete(mem, ids) if ids else []
            if ids:
                self._scrub(ids)
        log.info("mem0: forgot %d memories", len(gone))
        return len(gone)

    def _scrub(self, ids: list[str]) -> None:
        path = self.home / HISTORY_DB
        if path.exists():
            try:
                con = sqlite3.connect(str(path), timeout=10)
                try:
                    con.execute("PRAGMA secure_delete = ON")
                    tables = {r[0] for r in con.execute("SELECT name FROM sqlite_master WHERE type='table'")}
                    for table in ("history", "history_old"):
                        if table in tables:
                            cols = {r[1] for r in con.execute(f"PRAGMA table_info({table})")}
                            if "memory_id" in cols:
                                con.executemany(f"DELETE FROM {table} WHERE memory_id = ?", [(i,) for i in ids])
                    if "messages" in tables:
                        con.execute("DELETE FROM messages")
                    con.commit()
                finally:
                    con.close()
            except sqlite3.Error as e:
                log.warning("mem0: could not scrub the history database (%s)", type(e).__name__)
        for db in [path, *sorted((self.home / "qdrant").rglob("*.sqlite"))]:
            if db.exists() and not db.is_symlink():
                try:
                    con = sqlite3.connect(str(db), timeout=10, isolation_level=None)
                    try:
                        con.execute("VACUUM")
                    finally:
                        con.close()
                except sqlite3.Error as e:
                    log.warning("mem0: could not compact a database (%s)", type(e).__name__)


_shared: dict[str, OwnerMemory] = {}
_shared_lock = threading.Lock()


def shared(recall_cfg: RecallConfig, environ: Any = None) -> Optional[OwnerMemory]:
    """The daemon's one OwnerMemory (a local Qdrant store takes one opener), or None when memory or the
    index is switched off, or mem0ai is not installed."""
    cfg = configured(recall_cfg, environ)
    if not cfg.enabled:
        return None
    if not available():
        log.warning("mem0: mem0ai is not installed — memory search stays records and transcripts")
        return None
    key = str(cfg.home)
    with _shared_lock:
        if key not in _shared:
            _shared[key] = OwnerMemory(cfg)
        return _shared[key]
