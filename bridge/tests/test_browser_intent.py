"""The Laya request-outcome delegate (browser_intent.expected_outcome).

Code reads the literal arguments (site, query, result number); Laya picks only the kind. The model is stubbed
here; the one real-model test runs only when laya-mlx and the browser checkpoint are already on this Mac.
"""

from __future__ import annotations

import sys
import threading
import types
from pathlib import Path

import pytest

from eve_bridge import browser_intent as bi


class FakeEngine:
    """Answers every question with a fixed probability table, the way laya-mlx shapes a choice answer."""

    def __init__(self, probabilities: dict[str, float]) -> None:
        self.probabilities = probabilities
        self.asked: list[tuple[str, dict]] = []

    def ask(self, goal: str, question: dict, state: dict) -> dict:
        self.asked.append((goal, question))
        options = list(question["criteria"])
        probabilities = {key: self.probabilities.get(key, 0.0) for key in options}
        rest = 1.0 - sum(probabilities.values())
        if rest > 0:
            spare = [key for key in options if key not in self.probabilities]
            for key in spare:
                probabilities[key] = rest / len(spare)
        choice = max(probabilities, key=probabilities.__getitem__)
        return {"type": "choice", "choice": choice, "probabilities": probabilities, "confidence": 0.9}


def pick(kind: str, p: float = 0.9) -> FakeEngine:
    key = "unsupported" if kind == "none" else kind
    return FakeEngine({key: p})


def outcome(request: str, engine: FakeEngine) -> bi.RequestOutcome | None:
    return bi.expected_outcome(request, engine=engine, environ={})


# ---- literal arguments, all code -------------------------------------------------------------------------

def test_literal_arguments_come_from_the_words() -> None:
    a = bi.literal_arguments("search you tube for lofi beats")
    assert (a.site.host, a.query, a.n) == ("youtube.com", "lofi beats", None)
    a = bi.literal_arguments("search for alan turing on wiki pedia and open the first result")
    assert (a.site.host, a.query, a.n) == ("wikipedia.org", "alan turing", 1)
    a = bi.literal_arguments("open git hub")
    assert (a.site.host, a.query, a.n) == ("github.com", None, None)
    a = bi.literal_arguments("open the 3rd result")
    assert (a.site, a.query, a.n) == (None, None, 3)
    a = bi.literal_arguments("play the second video")
    assert a.n == 2
    a = bi.literal_arguments("Search for \"rust async\".")
    assert a.query == "rust async"


# ---- Laya picks the kind ---------------------------------------------------------------------------------

def test_search_carries_the_literal_query_and_site() -> None:
    r = outcome("search you tube for lofi beats", pick("search"))
    assert r == bi.RequestOutcome("search", "youtube.com", "lofi beats", None, 0.9, "laya")
    assert r.latency_ms >= 0


def test_open_site_and_open_result() -> None:
    r = outcome("open git hub", pick("open_site"))
    assert (r.kind, r.site_host, r.query, r.n, r.source) == ("open_site", "github.com", None, None, "laya")
    r = outcome("search for alan turing on wikipedia and open the first result", pick("open_result"))
    assert (r.kind, r.site_host, r.query, r.n) == ("open_result", "wikipedia.org", "alan turing", 1)


def test_none_is_an_answer_not_an_abstention() -> None:
    r = outcome("search for cats and email them to my boss", pick("none"))
    assert r is not None and r.kind == "none" and r.source == "laya"


def test_laya_is_asked_in_its_native_shape() -> None:
    engine = pick("search")
    outcome("search for lofi beats", engine)
    goal, question = engine.asked[0]
    assert goal == "search for lofi beats"
    assert question["type"] == "choice"
    assert question["instructions"]["goal"] == "search for lofi beats"
    assert set(question["criteria"]) == {"search", "open_result", "unsupported"}


def test_only_kinds_the_arguments_can_carry_are_offered() -> None:
    def keys(request: str) -> set[str]:
        return set(bi.offered(bi.literal_arguments(request)))
    assert keys("open git hub") == {"open_site", "unsupported"}
    assert keys("search you tube for cats") == {"search", "open_result", "unsupported"}
    assert keys("search you tube for cats and play the first video") == {"open_result", "unsupported"}
    assert keys("open the second result") == {"open_result", "unsupported"}


def test_no_arguments_at_all_is_none_from_code_without_asking_laya() -> None:
    engine = pick("search")
    r = outcome("turn up the volume", engine)
    assert r is not None and (r.kind, r.source) == ("none", "code")
    assert engine.asked == []


# ---- abstentions -----------------------------------------------------------------------------------------

@pytest.mark.parametrize("request_, kind", [
    ("open git hub", "search"),                       # search with no query
    ("search for lofi beats", "open_site"),           # open a site that was never named
    ("search for lofi beats", "open_result"),         # open a result with no number
    ("search you tube for cats", "open_site"),        # opening the site drops the spoken query
])
def test_a_kind_the_arguments_cannot_carry_abstains(request_: str, kind: str) -> None:
    assert outcome(request_, pick(kind)) is None


def test_the_gate_abstains_on_low_confidence_or_a_thin_margin() -> None:
    assert outcome("search for lofi beats", FakeEngine({"search": 0.45, "open_result": 0.2})) is None
    assert outcome("search for lofi beats", FakeEngine({"search": 0.52, "open_result": 0.46})) is None
    assert outcome("search for lofi beats", FakeEngine({"search": 0.55, "open_result": 0.40})) is not None


def test_a_malformed_answer_abstains() -> None:
    class Broken:
        def ask(self, goal, question, state):
            return {"choice": "search", "probabilities": {"search": 2.0}}
    assert bi.expected_outcome("search for lofi beats", engine=Broken(), environ={}) is None

    class Raises:
        def ask(self, goal, question, state):
            raise RuntimeError("metal went away")
    assert bi.expected_outcome("search for lofi beats", engine=Raises(), environ={}) is None


def test_off_switch() -> None:
    engine = pick("search")
    assert bi.expected_outcome("search for cats", engine=engine, environ={"EVE_BROWSER_INTENT": "off"}) is None
    assert engine.asked == []
    assert bi.mode({}) == "laya" and bi.mode({"EVE_BROWSER_INTENT": "LAYA"}) == "laya"
    assert bi.mode({"EVE_BROWSER_INTENT": "jev"}) == "off"


# ---- the real engine: lazy, warm once, abstains cleanly ------------------------------------------------------

def test_abstains_cleanly_when_laya_mlx_is_absent(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setitem(sys.modules, "laya_mlx", None)
    engine = bi.LayaKindEngine()
    assert bi.expected_outcome("search you tube for cats", engine=engine, environ={}) is None
    assert not engine.loaded


def test_a_failed_load_abstains_and_is_not_retried_every_call(monkeypatch: pytest.MonkeyPatch) -> None:
    calls = []
    fake = types.ModuleType("laya_mlx")

    def load(*args, **kwargs):
        calls.append(kwargs)
        raise OSError("weights missing")
    fake.load = load
    mlx = types.ModuleType("mlx")
    core = types.ModuleType("mlx.core")
    core.set_cache_limit = lambda n: calls.append({"cache": n})
    mlx.core = core
    monkeypatch.setitem(sys.modules, "laya_mlx", fake)
    monkeypatch.setitem(sys.modules, "mlx", mlx)
    monkeypatch.setitem(sys.modules, "mlx.core", core)
    monkeypatch.setattr(bi, "_cached_checkpoint", lambda: "/nowhere")
    engine = bi.LayaKindEngine()
    for _ in range(3):
        assert bi.expected_outcome("search you tube for cats", engine=engine, environ={}) is None
    loads = [c for c in calls if "cache" not in c]
    assert len(loads) == 1
    assert {"cache": bi.MLX_CACHE_LIMIT_BYTES} in calls
    assert loads[0]["subfolder"] == bi.BROWSER_SUBFOLDER and loads[0]["revision"] == bi.BROWSER_REVISION


def test_not_cached_abstains_without_the_network(monkeypatch: pytest.MonkeyPatch) -> None:
    fake = types.ModuleType("laya_mlx")
    fake.load = lambda *a, **k: pytest.fail("must not load (or download) an uncached checkpoint")
    monkeypatch.setitem(sys.modules, "laya_mlx", fake)
    monkeypatch.setattr(bi, "_cached_checkpoint", lambda: None)
    assert bi.expected_outcome("search for cats", engine=bi.LayaKindEngine(), environ={}) is None


def test_warm_loads_once_under_concurrency(monkeypatch: pytest.MonkeyPatch) -> None:
    loads = []

    class Agent:
        cfg = {"max_len": 512, "head_max_len": 192}

        def predict(self, state, questions):
            (qid, q), = questions.items()
            probs = {k: (0.94 if k == "search" else 0.06 / (len(q["criteria"]) - 1)) for k in q["criteria"]}
            return {"answers": {qid: {"type": "choice", "choice": "search", "probabilities": probs}}}

    fake = types.ModuleType("laya_mlx")

    def load(*args, **kwargs):
        loads.append(1)
        return Agent()
    fake.load = load
    mlx = types.ModuleType("mlx")
    core = types.ModuleType("mlx.core")
    core.set_cache_limit = lambda n: None
    mlx.core = core
    monkeypatch.setitem(sys.modules, "laya_mlx", fake)
    monkeypatch.setitem(sys.modules, "mlx", mlx)
    monkeypatch.setitem(sys.modules, "mlx.core", core)
    monkeypatch.setattr(bi, "_cached_checkpoint", lambda: "/cached")
    monkeypatch.setattr(bi.LayaKindEngine, "_prefix_fits", lambda self, q: True)
    engine = bi.LayaKindEngine()
    results = []
    threads = [threading.Thread(target=lambda: results.append(
        bi.expected_outcome("search for lofi beats", engine=engine, environ={}))) for _ in range(8)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    assert len(loads) == 1
    assert all(r is not None and r.kind == "search" for r in results)
    assert len(bi.recent_latencies()) >= 8


# ---- the real checkpoint, when it is already on this Mac -------------------------------------------------------

def _weights_cached() -> bool:
    try:
        import laya_mlx  # noqa: F401
    except Exception:
        return False
    path = bi._cached_checkpoint()
    return bool(path) and (Path(path) / bi.BROWSER_SUBFOLDER / "model.safetensors").exists()


@pytest.mark.skipif(not _weights_cached(), reason="laya-mlx or the cklxx/laya-browser checkpoint is not cached")
def test_the_real_checkpoint_on_three_obvious_requests() -> None:
    engine = bi.LayaKindEngine()
    expected = {
        "search YouTube for lofi beats": "search",
        "open git hub": "open_site",
        "search wikipedia for alan turing and open the first result": "open_result",
    }
    for request, kind in expected.items():
        r = bi.expected_outcome(request, engine=engine, environ={})
        assert r is not None and r.kind == kind, (request, r)
        assert r.source == "laya" and r.latency_ms > 0
